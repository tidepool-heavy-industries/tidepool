//! Owner-local observations for resident publication currency histories.
use super::*;

pub(in crate::session) fn assert_reused_projection(accepted: &AcceptedDeclarationPublication) {
    assert!(matches!(
        accepted.receipt,
        PublicationEvidence::ReusedProjection { .. }
    ));
}

pub(in crate::session) fn assert_preserved_certificate(
    accepted: AcceptedDeclarationPublication,
    transition: impl FnOnce(AcceptedDeclarationPublication) -> super::super::CertifiedPublication,
) -> super::super::CertifiedPublication {
    let reservation = accepted.reserved_generation();
    let receipt = accepted.receipt.receipt().clone();
    let intent = accepted.intent().clone();
    let semantics = paired_version(&accepted.base.public);
    let result = transition(accepted);
    let super::super::CertifiedPublication::Declarations(after) = &result else {
        panic!("declaration certificate")
    };
    assert_eq!(after.reserved_generation(), reservation);
    assert!(Arc::ptr_eq(after.receipt.receipt(), &receipt));
    assert!(Arc::ptr_eq(after.intent(), &intent));
    assert_eq!(paired_version(&after.base.public), semantics);
    result
}

pub(in crate::session) fn assert_stale_baseline(
    session: &PersistentSession,
    base: &PublicManifestBase,
) {
    assert!(matches!(
        session.declaration_baseline_is_current(base),
        Err(SessionError::StaleStagedDeclaration)
    ));
}
