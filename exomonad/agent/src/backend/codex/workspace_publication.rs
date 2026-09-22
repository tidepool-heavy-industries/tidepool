//! Workspace publication through the already-bound native controller socket.

use std::{num::NonZeroU64, path::PathBuf};

use crate::interactive::{PublicationOperation, PublicationReply};
use crate::{AgentBackendError, QueueReadyThread};

use codex_shoal_protocol::BoundReply;
use codex_shoal_protocol::WorkspaceProcessIdentity as Identity;
use codex_shoal_protocol::WorkspacePublicationOperation as Operation;
use codex_shoal_protocol::WorkspacePublicationReply as Reply;
use codex_shoal_protocol::WorkspacePublicationRequest as Request;

use super::controller;

pub(super) async fn request(
    thread: &QueueReadyThread,
    sequence: NonZeroU64,
    operation: PublicationOperation,
) -> Result<PublicationReply, AgentBackendError> {
    let binding = match controller::binding(thread) {
        Ok(binding) => binding,
        Err(controller::Failure::NotSubmitted(detail)) => {
            return Ok(PublicationReply::Unavailable { detail });
        }
        Err(controller::Failure::Unconfirmed(detail)) => return Err(unconfirmed(detail)),
    };
    let response = controller::post(
        thread,
        codex_shoal_protocol::WORKSPACE_PUBLICATION_PATH,
        &Request {
            binding: binding.clone(),
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
        },
        codex_shoal_protocol::MAX_WORKSPACE_REPLY_BYTES,
    )
    .await;
    let response = match response {
        Ok(response) => response,
        Err(controller::Failure::NotSubmitted(detail)) => {
            return Ok(PublicationReply::Unavailable { detail });
        }
        Err(controller::Failure::Unconfirmed(detail)) => return Err(unconfirmed(detail)),
    };
    if response.status == hyper::StatusCode::NOT_FOUND {
        return Ok(PublicationReply::Unavailable {
            detail: "native controller has no publication support".into(),
        });
    }
    if !response.status.is_success() {
        return Err(unconfirmed(format!(
            "native publication HTTP {}",
            response.status
        )));
    }
    let reply: BoundReply<Reply<PathBuf>> =
        serde_json::from_slice(&response.body).map_err(unconfirmed)?;
    controller::validate_reply_binding(&binding, &reply.binding).map_err(unconfirmed)?;
    // Credentials belong to the same connection carrying the receipt. A PID
    // serialized by the native process is local to its PID namespace.
    Ok(match reply.payload {
        Reply::Ready {
            pid,
            start_ticks,
            mount_namespace_inode,
            cgroup_path,
        } if pid > 0 && mount_namespace_inode > 0 && cgroup_path.is_absolute() => {
            PublicationReply::Ready {
                peer_pid: response.peer_pid,
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
