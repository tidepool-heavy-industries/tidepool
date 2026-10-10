//! Shared ownership of one runtime-authenticated successful result.

use std::sync::Arc;

use tidepool_repr::SessionId;
use tidepool_runtime::session::{resident::BindingLease, RootCustody, RuntimeResultPublication};
use tidepool_toolchain::checked_cell::CanonicalInputTypeWitness;

use crate::{request::ReplyError, ActorRef};

/// Issuance fixes the destination independently of later cleanup-owner handoff.
pub(crate) struct RequestResultDestination {
    issuer: ActorRef,
    session: SessionId,
    type_witness: Arc<CanonicalInputTypeWitness>,
    bindings: BindingLease,
}

impl RequestResultDestination {
    pub(crate) fn new(
        issuer: ActorRef,
        session: SessionId,
        type_witness: Arc<CanonicalInputTypeWitness>,
        bindings: BindingLease,
    ) -> Self {
        Self { issuer, session, type_witness, bindings }
    }

    pub(crate) fn issuer(&self) -> ActorRef { self.issuer }
    pub(crate) fn session(&self) -> SessionId { self.session }
    pub(crate) fn type_witness(&self) -> &Arc<CanonicalInputTypeWitness> { &self.type_witness }
    pub(crate) fn bindings(&self) -> &BindingLease { &self.bindings }
}

impl std::fmt::Debug for RequestResultDestination {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("RequestResultDestination")
            .field("issuer", &self.issuer)
            .field("session", &self.session)
            .field("type_witness", &self.type_witness)
            .finish_non_exhaustive()
    }
}

/// The runtime keeps the value and witness inseparable. Construction proves the
/// canonical root lives in the admitted issuer's machine, under its ROOT owner.
#[derive(Debug)]
pub(crate) struct OwnedResultSnapshot {
    publication: Arc<RuntimeResultPublication>,
    destination: Arc<RequestResultDestination>,
}

impl OwnedResultSnapshot {
    pub(crate) fn incorporated(
        publication: RuntimeResultPublication,
        destination: Arc<RequestResultDestination>,
    ) -> Result<Arc<Self>, ReplyError> {
        if publication.type_witness() != destination.type_witness() {
            return Err(ReplyError::ReplyResultTypeMismatch);
        }
        if !publication.belongs_to_bindings(destination.bindings()) {
            return Err(ReplyError::ReplyResultUnavailable);
        }
        Ok(Arc::new(Self { publication: Arc::new(publication), destination }))
    }

    pub(crate) fn value(&self) -> &RootCustody { self.publication.custody() }
    pub(crate) fn type_witness(&self) -> &Arc<CanonicalInputTypeWitness> {
        self.publication.type_witness()
    }
    pub(crate) fn session(&self) -> SessionId { self.destination.session() }
    pub(crate) fn publication(&self) -> &RuntimeResultPublication { &self.publication }
    pub(crate) fn publication_owner(&self) -> Arc<RuntimeResultPublication> {
        Arc::clone(&self.publication)
    }
    pub(crate) fn belongs_to(&self, destination: &Arc<RequestResultDestination>) -> bool {
        Arc::ptr_eq(&self.destination, destination)
    }
}
