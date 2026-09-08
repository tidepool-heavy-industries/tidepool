//! One live native write-admission transaction per workspace, not per layer.

use std::io;
use std::num::NonZeroU64;

use tidepool_agent::interactive::{PublicationIdentity, PublicationOperation, PublicationReply};
use tidepool_agent::{InteractiveAgentBackend, QueueReadyThread};
use tidepool_node::MountNamespace;

#[derive(Default)]
pub(super) struct WorkspacePublication {
    completed: u64,
    pending: Option<Pending>,
}

struct Pending {
    sequence: NonZeroU64,
    identity: Option<PublicationIdentity>,
}

pub(super) enum Admission {
    Ready(MountNamespace),
    Busy,
    Unavailable(String),
}

impl WorkspacePublication {
    pub(super) fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn has_identity(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.identity.is_some())
    }

    /// Retain the sequence before sending. A lost reply never authorizes a
    /// different request, and host death ends the wave rather than replaying it.
    pub(super) async fn begin(
        &mut self,
        backend: &dyn InteractiveAgentBackend,
        thread: &QueueReadyThread,
    ) -> io::Result<Admission> {
        if self.pending.is_none() {
            let sequence = self
                .completed
                .checked_add(1)
                .and_then(NonZeroU64::new)
                .ok_or_else(|| io::Error::other("workspace publication sequence exhausted"))?;
            self.pending = Some(Pending {
                sequence,
                identity: None,
            });
        }
        let pending = self.pending.as_mut().expect("publication reserved above");
        match backend
            .workspace_publication(
                thread,
                pending.sequence,
                PublicationOperation::Begin {
                    expected: pending.identity,
                },
            )
            .await
            .map_err(io::Error::other)?
        {
            PublicationReply::Ready {
                pid,
                start_ticks,
                mount_namespace_inode,
                ..
            } => {
                let identity = PublicationIdentity {
                    pid,
                    start_ticks,
                    mount_namespace_inode,
                };
                if pending.identity.is_some_and(|prior| prior != identity) {
                    return Err(io::Error::other(
                        "workspace publication native owner changed",
                    ));
                }
                pending.identity = Some(identity);
                // Retain identity even when descriptor capture fails: finish
                // still has to release precisely this native admission.
                MountNamespace::capture_matching(pid, start_ticks, mount_namespace_inode)
                    .map(Admission::Ready)
            }
            PublicationReply::Busy if pending.identity.is_none() => {
                self.pending = None;
                Ok(Admission::Busy)
            }
            PublicationReply::Unavailable { detail } if pending.identity.is_none() => {
                self.pending = None;
                Ok(Admission::Unavailable(detail))
            }
            PublicationReply::Settled => {
                self.completed = pending.sequence.get();
                self.pending = None;
                Ok(Admission::Unavailable(
                    "previous publication already settled".into(),
                ))
            }
            reply => Err(io::Error::other(format!(
                "workspace admission is unconfirmed: {reply:?}"
            ))),
        }
    }

    /// The workspace owner must first reconcile all possibly started mount
    /// operations. This function releases admission, never guesses mount state.
    pub(super) async fn finish(
        &mut self,
        backend: &dyn InteractiveAgentBackend,
        thread: &QueueReadyThread,
    ) -> io::Result<()> {
        let Some(pending) = &self.pending else {
            return Ok(());
        };
        let identity = pending
            .identity
            .ok_or_else(|| io::Error::other("workspace admission reply is still unknown"))?;
        match backend
            .workspace_publication(
                thread,
                pending.sequence,
                PublicationOperation::Finish { expected: identity },
            )
            .await
            .map_err(io::Error::other)?
        {
            PublicationReply::Settled => {
                self.completed = pending.sequence.get();
                self.pending = None;
                Ok(())
            }
            reply => Err(io::Error::other(format!(
                "workspace completion is unconfirmed: {reply:?}"
            ))),
        }
    }
}
