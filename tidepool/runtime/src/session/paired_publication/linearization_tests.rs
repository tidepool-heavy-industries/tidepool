//! Independent owner retries retain their actual completed result.

use super::*;
use crate::session::recovery_test_fixture::RecoveryFixture;
use crate::session::{
    ModuleEnv, PublicManifestCommit, PublicationDecision, PublicationPhase, SessionId,
};

#[test]
fn fixed_sibling_intents_restage_after_observation_and_domain_changes_without_reexecution() {
    let root = tempfile::tempdir().unwrap();
    let mut lib = SessionLib::open(
        SessionId(4484),
        root.path().join("session"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let recovery = RecoveryFixture::create(root.path());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
    let public_a = session.mint_isolated_scope();
    let public_b = session.mint_isolated_scope();
    let owner_a =
        RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/a").unwrap(), 1).unwrap();
    let owner_b =
        RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse("root/b").unwrap(), 1).unwrap();
    session
        .initialize_durable_public_scope(owner_a.clone(), public_a)
        .unwrap();
    session
        .initialize_durable_public_scope(owner_b.clone(), public_b)
        .unwrap();
    let a = session
        .begin_durable_private_execution(&owner_a, public_a)
        .unwrap();
    let b = session
        .begin_durable_private_execution(&owner_b, public_b)
        .unwrap();
    let value_a = crate::session::prepared::tests::evaluated_publication_fixture(
        &mut session,
        "observationA",
        720,
    );
    let id_a = value_a.id;
    session.bind_in(a.private_scope(), value_a).unwrap();
    session.save_observation(id_a, &[]);
    let intent_a = session.freeze_private_execution(&a).unwrap();
    let before_a = session
        .public_visibility_snapshot_in(a.private_scope())
        .unwrap();
    let ExecutionPublication::Bindings(base_a) = session
        .restage_execution_publication(owner_a.clone(), intent_a.clone())
        .unwrap()
    else {
        panic!("binding-only publication");
    };
    let staged_a = base_a.stage().unwrap();

    // The sibling completes once after A has frozen and staged its exact
    // result. Its observation invalidates caches, and its domain commit makes A's
    // staged graph stale; neither event changes A's private semantic view.
    let value_b = crate::session::prepared::tests::evaluated_publication_fixture(
        &mut session,
        "observationB",
        721,
    );
    let id_b = value_b.id;
    session.bind_in(b.private_scope(), value_b).unwrap();
    session.save_observation(id_b, &[]);
    let intent_b = session.freeze_private_execution(&b).unwrap();
    let ExecutionPublication::Bindings(base_b) = session
        .restage_execution_publication(owner_b, intent_b)
        .unwrap()
    else {
        panic!("binding-only publication");
    };
    assert!(matches!(
        session
            .attempt_public_manifest(base_b.stage().unwrap(), &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    assert_eq!(
        session
            .public_visibility_snapshot_in(a.private_scope())
            .unwrap(),
        before_a
    );
    let decision_a = PublicationDecision::new();
    assert!(matches!(
        session
            .attempt_public_manifest(staged_a, &decision_a)
            .unwrap(),
        crate::session::PublicationAttempt::RestageMetadata(_)
    ));
    assert_eq!(decision_a.phase(), PublicationPhase::Running);
    let ExecutionPublication::Bindings(retry_a) = session
        .restage_execution_publication(owner_a, intent_a.clone())
        .unwrap()
    else {
        panic!("same binding-only intent");
    };
    assert!(matches!(
        session
            .attempt_public_manifest(retry_a.stage().unwrap(), &decision_a)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
    assert_eq!(
        session.resolve_in(public_a, "observationA").unwrap().id,
        id_a
    );
    assert_eq!(
        session.resolve_in(public_b, "observationB").unwrap().id,
        id_b
    );
    assert!(Arc::ptr_eq(
        &intent_a,
        &session.freeze_private_execution(&a).unwrap()
    ));
}
