//! Workspace publication through the already-bound native controller socket.

use std::{num::NonZeroU64, path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

use crate::interactive::{PublicationOperation, PublicationReply};
use crate::{AgentBackendError, QueueReadyThread};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Request<'a> {
    thread_id: &'a str,
    sequence: NonZeroU64,
    operation: Operation,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_identity: Option<Identity>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    pid: u32,
    start_ticks: u64,
    mount_namespace_inode: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum Operation {
    Begin,
    Finish,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum Reply {
    Ready {
        pid: u32,
        #[serde(rename = "startTicks")]
        start_ticks: u64,
        #[serde(rename = "mountNamespaceInode")]
        mount_namespace_inode: u64,
        #[serde(rename = "cgroupPath")]
        cgroup_path: PathBuf,
    },
    Settled,
    Busy,
    Conflict,
    Unavailable {
        reason: String,
    },
}

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
    let client = reqwest::Client::builder()
        .unix_socket(socket)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .http1_only()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(unconfirmed)?;
    let mut response = client
        .post("http://localhost/v1/workspace/publication")
        .json(&Request {
            thread_id: &thread.id().0,
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
        .send()
        .await
        .map_err(unconfirmed)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(PublicationReply::Unavailable {
            detail: "native controller has no publication support".into(),
        });
    }
    response.error_for_status_ref().map_err(unconfirmed)?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(unconfirmed)? {
        if body.len() + chunk.len() > 16 * 1024 {
            return Err(unconfirmed("oversized publication reply"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(match serde_json::from_slice(&body).map_err(unconfirmed)? {
        Reply::Ready {
            pid,
            start_ticks,
            mount_namespace_inode,
            cgroup_path,
        } if pid > 0 && mount_namespace_inode > 0 && cgroup_path.is_absolute() => {
            PublicationReply::Ready {
                pid,
                start_ticks,
                mount_namespace_inode,
                cgroup_path,
            }
        }
        Reply::Ready { .. } => return Err(unconfirmed("invalid native publication identity")),
        Reply::Settled => PublicationReply::Settled,
        Reply::Busy => PublicationReply::Busy,
        Reply::Conflict => PublicationReply::Conflict,
        Reply::Unavailable { reason } => PublicationReply::Unavailable { detail: reason },
    })
}

fn unconfirmed(error: impl std::fmt::Display) -> AgentBackendError {
    AgentBackendError::BackendUnavailable {
        detail: format!("native workspace publication is unconfirmed: {error}"),
    }
}

#[cfg(test)]
#[path = "workspace_publication_tests.rs"]
mod tests;
