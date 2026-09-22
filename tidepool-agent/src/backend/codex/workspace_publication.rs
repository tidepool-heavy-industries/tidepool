//! Workspace publication through the already-bound native controller socket.

use std::{num::NonZeroU64, path::PathBuf, time::Duration};

use crate::interactive::{PublicationOperation, PublicationReply};
use crate::{AgentBackendError, QueueReadyThread};

use codex_shoal_protocol::WorkspaceProcessIdentity as Identity;
use codex_shoal_protocol::WorkspacePublicationOperation as Operation;
use codex_shoal_protocol::WorkspacePublicationReply as Reply;
use codex_shoal_protocol::WorkspacePublicationRequest as Request;

pub(super) async fn request(
    thread: &QueueReadyThread,
    sequence: NonZeroU64,
    operation: PublicationOperation,
) -> Result<PublicationReply, AgentBackendError> {
    let Some(socket) = thread.input_control_socket() else {
        return Ok(PublicationReply::Unavailable {
            detail: "native controller socket is unavailable".into(),
        });
    };
    // Credentials belong to the same connection carrying the receipt. A PID
    // serialized by the native process is local to its PID namespace.
    tokio::time::timeout(Duration::from_secs(30), async {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .map_err(unconfirmed)?;
        let peer_pid = stream
            .peer_cred()
            .map_err(unconfirmed)?
            .pid()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
            .ok_or_else(|| unconfirmed("native publication peer PID unavailable"))?;
        let (mut client, connection) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .map_err(unconfirmed)?;
        let driver = tokio::spawn(connection);
        // Abort the driver if the request is cancelled or exceeds its deadline.
        struct Driver(tokio::task::JoinHandle<Result<(), hyper::Error>>);
        impl Drop for Driver {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _driver = Driver(driver);
        let body = serde_json::to_vec(&Request {
            thread_id: thread.id().0.clone(),
            sequence,
            operation: match operation {
                PublicationOperation::Begin { .. } => Operation::Begin,
                PublicationOperation::Finish { .. } => Operation::Finish,
            },
            expected_identity: match operation {
                PublicationOperation::Begin { expected } => expected,
                PublicationOperation::Finish { expected } => Some(expected),
            }
            .map(|identity| Identity {
                pid: identity.pid,
                start_ticks: identity.start_ticks,
                mount_namespace_inode: identity.mount_namespace_inode,
            }),
        })
        .map_err(unconfirmed)?;
        let request = hyper::Request::post(codex_shoal_protocol::WORKSPACE_PUBLICATION_PATH)
            .header("Host", "localhost")
            .header("Content-Type", "application/json")
            .body(http_body_util::Full::new(hyper::body::Bytes::from(body)))
            .map_err(unconfirmed)?;
        let response = client.send_request(request).await.map_err(unconfirmed)?;
        if response.status() == hyper::StatusCode::NOT_FOUND {
            return Ok(PublicationReply::Unavailable {
                detail: "native controller has no publication support".into(),
            });
        }
        if !response.status().is_success() {
            return Err(unconfirmed(format!(
                "native publication HTTP {}",
                response.status()
            )));
        }
        use http_body_util::BodyExt;
        let body = http_body_util::Limited::new(response.into_body(), 16 * 1024)
            .collect()
            .await
            .map_err(unconfirmed)?
            .to_bytes();
        Ok(
            match serde_json::from_slice::<Reply<PathBuf>>(&body).map_err(unconfirmed)? {
                Reply::Ready {
                    pid,
                    start_ticks,
                    mount_namespace_inode,
                    cgroup_path,
                } if pid > 0 && mount_namespace_inode > 0 && cgroup_path.is_absolute() => {
                    PublicationReply::Ready {
                        peer_pid,
                        pid,
                        start_ticks,
                        mount_namespace_inode,
                        cgroup_path,
                    }
                }
                Reply::Ready { .. } => {
                    return Err(unconfirmed("invalid native publication identity"))
                }
                Reply::Settled => PublicationReply::Settled,
                Reply::Busy => PublicationReply::Busy,
                Reply::Conflict => PublicationReply::Conflict,
                Reply::Unavailable { reason } => PublicationReply::Unavailable { detail: reason },
            },
        )
    })
    .await
    .map_err(unconfirmed)?
}

fn unconfirmed(error: impl std::fmt::Display) -> AgentBackendError {
    AgentBackendError::BackendUnavailable {
        detail: format!("native workspace publication is unconfirmed: {error}"),
    }
}

#[cfg(test)]
#[path = "workspace_publication_tests.rs"]
mod tests;
