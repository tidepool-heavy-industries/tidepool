use std::fmt;

use tidepool_repr::SessionId;
use tidepool_runtime::session::{ResidentHole, RootCustody};

use crate::ActorRef;

/// The installed one-message receiver for an exact actor incarnation.
///
/// The parked authored continuation consumes `next`; the rooted rank-N
/// handler consumes the next protocol request. Both have move-only Rust
/// custody and therefore live in the actor behavior, not in a parallel
/// program table; this imposes no linearity discipline on authored Haskell.
pub(crate) struct InstalledReceiver {
    pub(crate) site: u64,
    pub(crate) continuation: ResidentHole,
    pub(crate) handler: RootCustody,
}

pub(crate) struct KernelValue {
    pub(crate) continuation: ResidentHole,
    pub(crate) value: RootCustody,
}

pub(crate) enum ResidentOutbound {
    Call {
        target: ActorRef,
        continuation: ResidentHole,
        request: MailboxValue,
    },
    Cast {
        target: ActorRef,
        continuation: ResidentHole,
        request: MailboxValue,
    },
}

pub(crate) struct ResidentWaitRequest {
    pub(crate) target: ActorRef,
    pub(crate) continuation: ResidentHole,
}

/// One live Haskell value under exclusive machine-root custody.
///
/// The session tag lets the actor kernel reject a cross-machine delivery
/// before the custody token leaves its envelope. Dropping this value drops
/// [`RootCustody`], which queues the underlying root for release by its
/// originating resident session.
#[must_use = "a live mailbox value must be delivered or deliberately dropped"]
pub struct MailboxValue {
    session: SessionId,
    root: MailboxRoot,
}

enum MailboxRoot {
    Runtime(RootCustody),
    #[cfg(test)]
    Probe {
        _drop: DropProbe,
    },
}

impl MailboxValue {
    pub fn new(session: SessionId, custody: RootCustody) -> Self {
        Self {
            session,
            root: MailboxRoot::Runtime(custody),
        }
    }

    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Recover custody after the actor kernel has validated the destination
    /// session. This consumes the envelope's ownership token exactly once.
    pub fn into_custody(self) -> RootCustody {
        match self.root {
            MailboxRoot::Runtime(custody) => custody,
            #[cfg(test)]
            MailboxRoot::Probe { .. } => panic!("test root has no runtime custody"),
        }
    }

    #[cfg(test)]
    pub(crate) fn probe(
        session: SessionId,
        dropped: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            session,
            root: MailboxRoot::Probe {
                _drop: DropProbe(dropped),
            },
        }
    }
}

impl fmt::Debug for MailboxValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MailboxValue")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
struct DropProbe(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(test)]
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
