use std::fmt;

use tidepool_repr::SessionId;
use tidepool_runtime::session::{Parcel, ResidentHole, RootCustody};

use crate::ActorRef;

/// The installed one-message receiver for an exact actor incarnation.
///
/// The parked authored continuation consumes `next`; the rooted rank-N
/// handler consumes the next protocol request. Both have move-only Rust
/// owned handles and therefore live in the actor behavior, not in a parallel
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
    TryCall {
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

/// One live Haskell value crossing the mailbox, either already rooted under
/// an exclusive machine handle or still sealed in a detached [`Parcel`].
///
/// The session tag lets the actor kernel reject a cross-machine delivery
/// before a [`RootCustody`] handle ever leaves its envelope -- but only for
/// the `Runtime` form: a [`Parcel`] carries no machine affinity of its own
/// (see [`tidepool_codegen::prepared_program::evacuation`]'s module doc) and
/// is always deliverable, wherever it lands, by importing it into the
/// receiving machine instead. Dropping this value drops whatever it holds:
/// a `Runtime` root queues for release by its originating resident session;
/// a `Parcel`'s detached arena and payloads are simply freed -- it was never
/// registered with any machine's ledger, so nothing needs releasing there.
#[must_use = "a live mailbox value must be delivered or deliberately dropped"]
pub struct MailboxValue {
    session: SessionId,
    root: MailboxRoot,
}

enum MailboxRoot {
    Runtime(RootCustody),
    Parcel(Parcel),
    #[cfg(test)]
    Probe {
        _drop: DropProbe,
        kind: ProbeKind,
    },
}

/// Which real form a [`MailboxRoot::Probe`] stands in for -- the probe never
/// touches a machine, so this only needs to steer [`MailboxValue::deliver`]'s
/// branch, not carry any actual [`RootCustody`]/[`Parcel`] payload.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProbeKind {
    Runtime,
    Parcel,
}

/// [`MailboxValue::deliver`]'s accepted outcome: which machine-level
/// operation the receiving session must still perform. A `Runtime` value
/// arrives ready to mount; a `Parcel` still needs
/// [`tidepool_runtime::session::ResidentSession::import_parcel`] run against
/// the receiving machine before it has a [`RootCustody`] of its own. This is
/// the primitive a delivery site matches on to decide which operation to run.
pub enum MailboxDelivery {
    Runtime(RootCustody),
    Parcel(Parcel),
    #[cfg(test)]
    Probe(DropProbe),
}

/// [`MailboxValue::into_transfer`]'s decomposition: which primitive the
/// actor kernel's cross-machine transfer must run to move `self` into a new
/// destination machine. A `Runtime` custody still lives in its recorded
/// `session`'s machine and must be exported from there; a `Parcel` is
/// already detached and only needs importing into the destination.
pub(crate) enum MailboxTransfer {
    Runtime {
        session: SessionId,
        custody: RootCustody,
    },
    Parcel(Parcel),
    #[cfg(test)]
    ProbeRuntime {
        session: SessionId,
        drop: DropProbe,
    },
    #[cfg(test)]
    ProbeParcel(DropProbe),
}

impl fmt::Debug for MailboxDelivery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime(custody) => f.debug_tuple("Runtime").field(custody).finish(),
            Self::Parcel(_) => f.debug_tuple("Parcel").finish_non_exhaustive(),
            #[cfg(test)]
            Self::Probe(_) => f.debug_tuple("Probe").finish_non_exhaustive(),
        }
    }
}

/// [`MailboxValue::deliver`] rejected a `Runtime` value addressed to a
/// session other than `destination`: its handle belongs to a different
/// machine's ledger and was never at risk of leaving its envelope.
#[derive(Debug, thiserror::Error)]
#[error(
    "mailbox value from session {actual:?} cannot deliver into session {destination:?}: \
     a Runtime-rooted value crosses only into its own originating machine"
)]
pub struct ForeignMailboxValue {
    pub destination: SessionId,
    pub actual: SessionId,
}

impl MailboxValue {
    pub fn new(session: SessionId, custody: RootCustody) -> Self {
        Self {
            session,
            root: MailboxRoot::Runtime(custody),
        }
    }

    /// Wrap a detached [`Parcel`] for delivery. `session` is the value's
    /// nominal origin (mirrors [`Self::new`]) -- purely informational for
    /// this form, since [`Self::deliver`] never rejects a `Parcel` on tag
    /// mismatch the way it does a `Runtime` custody.
    pub fn parcel(session: SessionId, parcel: Parcel) -> Self {
        Self {
            session,
            root: MailboxRoot::Parcel(parcel),
        }
    }

    #[must_use]
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Recover the handle after the actor kernel has validated the destination
    /// session. This consumes the envelope's ownership token exactly once.
    pub fn into_custody(self) -> RootCustody {
        match self.root {
            MailboxRoot::Runtime(custody) => custody,
            MailboxRoot::Parcel(_) => panic!("parcel root has no runtime custody"),
            #[cfg(test)]
            MailboxRoot::Probe { .. } => panic!("test root has no runtime custody"),
        }
    }

    /// Decompose `self` for a cross-machine TRANSFER, before any destination
    /// is known to already hold it -- unlike [`Self::deliver`], which
    /// classifies a value that already belongs to (or, for a `Parcel`, is
    /// about to be imported into) one particular destination. A `Runtime`
    /// custody keeps its recorded origin `session`, since the caller (the
    /// actor kernel's `transfer_mailbox_value`) still has to check that
    /// machine out to export from it; a `Parcel` carries no machine affinity
    /// at all, so there is nothing to record beyond the parcel itself.
    pub(crate) fn into_transfer(self) -> MailboxTransfer {
        match self.root {
            MailboxRoot::Runtime(custody) => MailboxTransfer::Runtime {
                session: self.session,
                custody,
            },
            MailboxRoot::Parcel(parcel) => MailboxTransfer::Parcel(parcel),
            #[cfg(test)]
            MailboxRoot::Probe { _drop, kind } => match kind {
                ProbeKind::Runtime => MailboxTransfer::ProbeRuntime {
                    session: self.session,
                    drop: _drop,
                },
                ProbeKind::Parcel => MailboxTransfer::ProbeParcel(_drop),
            },
        }
    }

    /// Classify `self` for delivery into `destination`'s machine: a
    /// `Runtime` custody is rejected before its handle leaves the envelope
    /// when it was minted under a different session, exactly as before
    /// parcels existed; a `Parcel` is always accepted, regardless of tag --
    /// the caller imports it into `destination`'s machine to get a
    /// `RootCustody` of its own.
    pub fn deliver(self, destination: SessionId) -> Result<MailboxDelivery, ForeignMailboxValue> {
        match self.root {
            MailboxRoot::Runtime(custody) => {
                if self.session == destination {
                    Ok(MailboxDelivery::Runtime(custody))
                } else {
                    Err(ForeignMailboxValue {
                        destination,
                        actual: self.session,
                    })
                }
            }
            MailboxRoot::Parcel(parcel) => Ok(MailboxDelivery::Parcel(parcel)),
            #[cfg(test)]
            MailboxRoot::Probe { _drop, kind } => match kind {
                ProbeKind::Runtime if self.session == destination => {
                    Ok(MailboxDelivery::Probe(_drop))
                }
                ProbeKind::Runtime => Err(ForeignMailboxValue {
                    destination,
                    actual: self.session,
                }),
                ProbeKind::Parcel => Ok(MailboxDelivery::Probe(_drop)),
            },
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
                kind: ProbeKind::Runtime,
            },
        }
    }

    /// [`Self::probe`], but standing in for the `Parcel` form: a foreign
    /// session tag must not cause [`Self::deliver`] to reject it.
    #[cfg(test)]
    pub(crate) fn probe_parcel(
        session: SessionId,
        dropped: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> Self {
        Self {
            session,
            root: MailboxRoot::Probe {
                _drop: DropProbe(dropped),
                kind: ProbeKind::Parcel,
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
pub struct DropProbe(std::sync::Arc<std::sync::atomic::AtomicUsize>);

#[cfg(test)]
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn runtime_form_with_a_foreign_tag_is_rejected() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let value = MailboxValue::probe(SessionId(1), Arc::clone(&dropped));
        let error = value
            .deliver(SessionId(2))
            .expect_err("a Runtime value minted under session 1 must not deliver into session 2");
        assert_eq!(error.destination, SessionId(2));
        assert_eq!(error.actual, SessionId(1));
        // Rejection returns the value's session tag in the error, not the
        // value itself -- the probe's handle is gone with it, exactly as a
        // real `Runtime` custody's would be if this were not a probe.
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn runtime_form_with_a_matching_tag_is_accepted() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let value = MailboxValue::probe(SessionId(7), Arc::clone(&dropped));
        let delivery = value
            .deliver(SessionId(7))
            .expect("a Runtime value delivers into its own originating session");
        assert!(matches!(delivery, MailboxDelivery::Probe(_)));
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "the accepted probe is still held by the returned delivery, not yet dropped"
        );
        drop(delivery);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn parcel_form_with_a_foreign_tag_still_imports() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let value = MailboxValue::probe_parcel(SessionId(1), Arc::clone(&dropped));
        // Session 2 never minted this value -- for a `Runtime` root that
        // would be `ForeignMailboxValue`, but a parcel has no machine
        // affinity to be foreign to.
        let delivery = value
            .deliver(SessionId(2))
            .expect("a Parcel value is always deliverable, whatever its nominal origin tag");
        assert!(matches!(delivery, MailboxDelivery::Probe(_)));
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(delivery);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_dropped_parcel_form_releases_nothing_on_any_machine() {
        // The probe stands in for "no machine registration happened": its
        // `DropProbe` only ever counts the drop itself, never a release
        // call against a `PreparedEngine`/`ResidentSession` -- exactly what
        // a real, never-imported `Parcel`'s `Drop` does (it frees its own
        // detached arena and payloads; nothing was ever registered with a
        // machine's ledger for it to release there).
        let dropped = Arc::new(AtomicUsize::new(0));
        let value = MailboxValue::probe_parcel(SessionId(4), Arc::clone(&dropped));
        assert_eq!(dropped.load(Ordering::SeqCst), 0);
        drop(value);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn into_transfer_decomposes_a_runtime_value_with_its_recorded_session() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let value = MailboxValue::probe(SessionId(3), Arc::clone(&dropped));
        let MailboxTransfer::ProbeRuntime { session, drop } = value.into_transfer() else {
            panic!("expected a Runtime transfer form");
        };
        assert_eq!(session, SessionId(3));
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "still held, not yet dropped"
        );
        std::mem::drop(drop);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn into_transfer_decomposes_a_parcel_value_regardless_of_its_nominal_tag() {
        let dropped = Arc::new(AtomicUsize::new(0));
        // A `Parcel` value never needs its own session to transfer -- unlike
        // the `Runtime` case above, `into_transfer` carries no session for
        // this arm at all.
        let value = MailboxValue::probe_parcel(SessionId(9), Arc::clone(&dropped));
        let MailboxTransfer::ProbeParcel(drop) = value.into_transfer() else {
            panic!("expected a Parcel transfer form");
        };
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            0,
            "still held, not yet dropped"
        );
        std::mem::drop(drop);
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }
}
