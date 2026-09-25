//! Sole transport owner for operations sent to one challenged native controller.

use std::time::Duration;

use http_body_util::BodyExt;
use serde::Serialize;

use crate::{InteractiveSessionBinding, QueueReadyThread};

const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
const OPERATION_DEADLINE: Duration = crate::interactive::INPUT_CONTROL_DEADLINE;

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("operation was not submitted: {0}")]
    NotSubmitted(String),
    #[error("operation outcome is unconfirmed: {0}")]
    Unconfirmed(String),
}

#[derive(Debug)]
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
    // The connection driver runs detached from here on, deliberately outliving
    // this call's own operation deadline. The peer may need to cancel and await
    // settlement of the busy actor's own active turn before it can admit this
    // input (see the TUI's `InputSettlementGate::before_input`), which can take
    // longer than we are willing to keep the caller waiting. Aborting the
    // driver on our own timeout would truncate that in-flight exchange and
    // guarantee native never records the operation, so a later `query` would
    // see `Unknown` forever instead of eventually reconciling. We simply stop
    // waiting; the exchange itself is left to finish on its own.
    tokio::spawn(connection);
    let request = hyper::Request::post(path)
        .header("Host", "localhost")
        .header("Content-Type", "application/json")
        .body(http_body_util::Full::new(hyper::body::Bytes::from(body)))
        .map_err(|error| Failure::NotSubmitted(error.to_string()))?;
    // The exchange itself is also spawned rather than polled inline, and for
    // the same reason: on timeout we drop only our own `JoinHandle`, which
    // detaches the task instead of cancelling it, so `client.send_request`
    // keeps running to whatever conclusion the peer reaches.
    let exchange = tokio::spawn(async move {
        let response = client
            .send_request(request)
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status();
        let body = http_body_util::Limited::new(response.into_body(), response_limit)
            .collect()
            .await
            .map_err(|error| format!("reply exceeds {response_limit} byte limit: {error}"))?
            .to_bytes()
            .to_vec();
        Ok::<_, String>((status, body))
    });
    match tokio::time::timeout(operation_deadline, exchange).await {
        Ok(Ok(Ok((status, body)))) => Ok(Reply {
            status,
            body,
            peer_pid,
        }),
        Ok(Ok(Err(error))) => Err(Failure::Unconfirmed(error)),
        Ok(Err(join_error)) => Err(Failure::Unconfirmed(join_error.to_string())),
        Err(_) => Err(Failure::Unconfirmed(format!(
            "operation exceeded {operation_deadline:?}; the exchange continues in the background \
             and its outcome, if any, is reconciled by a later query rather than repeated here"
        ))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn socket_thread(socket: std::path::PathBuf) -> QueueReadyThread {
        QueueReadyThread::new(crate::BackendThreadId("thread".into()))
            .with_input_control(Some(socket))
            .with_challenged_session_binding(Some(InteractiveSessionBinding {
                launch_id: "launch-1".into(),
                instance_id: "instance-2".into(),
                generation: std::num::NonZeroU64::new(7).unwrap(),
                nonce: "nonce-3".into(),
            }))
    }

    /// A client-side operation timeout must stop OUR wait without severing the
    /// peer's own in-flight exchange. The busy actor's own transport may need
    /// to cancel and settle its active turn before it can admit new input,
    /// which can legitimately outrun our patience; tearing the connection down
    /// under it would guarantee the operation is lost rather than merely slow.
    /// This proves the request still reaches the peer intact, and completes on
    /// the peer's own schedule, even though the client already gave up.
    #[tokio::test]
    async fn operation_timeout_detaches_the_exchange_instead_of_severing_it() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("input.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body = loop {
                let mut chunk = [0_u8; 4096];
                let size = stream.read(&mut chunk).await.unwrap();
                assert!(size > 0, "connection closed before the request was read");
                bytes.extend_from_slice(&chunk[..size]);
                let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                    continue;
                };
                let header = std::str::from_utf8(&bytes[..end]).unwrap();
                let length: usize = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|value| value.trim().parse().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break bytes[end + 4..end + 4 + length].to_vec();
                }
            };
            // Reply well after the client's own operation deadline. A
            // severed connection would fail this read or the reply write;
            // succeeding here proves the exchange was left running.
            tokio::time::sleep(Duration::from_millis(200)).await;
            let response = b"{}";
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(response).await.unwrap();
            body
        });

        let thread = socket_thread(socket);
        let started = tokio::time::Instant::now();
        let result = post_with_deadlines(
            &thread,
            "/v1/input/control",
            &serde_json::json!({"hello": "world"}),
            4096,
            CONNECT_DEADLINE,
            Duration::from_millis(50),
        )
        .await;
        let elapsed = started.elapsed();

        assert!(matches!(result, Err(Failure::Unconfirmed(_))), "{result:?}");
        assert!(
            elapsed < Duration::from_millis(180),
            "the caller should stop waiting at its own deadline, not the peer's: {elapsed:?}"
        );

        let request_body = server.await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request_body).unwrap(),
            serde_json::json!({"hello": "world"}),
            "the peer must still receive the full request despite the client giving up"
        );
    }
}
