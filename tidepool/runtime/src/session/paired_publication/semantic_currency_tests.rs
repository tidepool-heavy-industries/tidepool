use super::tests::{accepted, commit_certified_source};
use super::*;
use crate::session::{
    CertifiedPublication, ModuleEnv, PublicManifestCommit, PublicationAttempt, PublicationDecision,
    PublicationPhase, RecoveryDomainId, RunRecoveryStore, SessionId,
};

#[track_caller]
fn owner(name: &str) -> RecoveryPublicOwner {
    let path =
        tidepool_repr::ActorPath::parse(name).expect("parse recovery public owner actor path");
    RecoveryPublicOwner::new(&path, 1).expect("issue recovery public owner")
}

#[test]
fn reservations_refill_and_unrelated_private_facts_preserve_certificate_through_reopen() {
    let root = tempfile::tempdir().unwrap();
    let mut lib = SessionLib::open(
        SessionId(9891),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
    let public_a = session.mint_scope(ScopeId::ROOT).unwrap();
    let public_b = session.mint_scope(ScopeId::ROOT).unwrap();
    let owner_a = owner("root/currency-a");
    let owner_b = owner("root/currency-b");
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
    let original_a = commit_certified_source(
        &mut session,
        a.private_scope(),
        "answerA :: Int\nanswerA = 11",
    );
    let original_b = commit_certified_source(
        &mut session,
        b.private_scope(),
        "answerB :: Int\nanswerB = 22",
    );
    let intent_a = session.freeze_execution_intent(&a, vec![], vec![]).unwrap();
    let intent_b = session.freeze_execution_intent(&b, vec![], vec![]).unwrap();
    let base_a = session
        .restage_declaration_publication(owner_a.clone(), intent_a.clone())
        .unwrap();
    let revision = session.lib().log.publication_revision();
    let semantic_a = paired_version(&base_a.public);
    let base_b = session
        .restage_declaration_publication(owner_b.clone(), intent_b)
        .unwrap();
    assert_eq!(session.lib().log.publication_revision(), revision);
    assert_eq!(paired_version(&base_a.public), semantic_a);
    let a = accepted(base_a);
    let b = accepted(base_b);
    assert!(matches!(
        a.receipt,
        PublicationEvidence::ReusedProjection { .. }
    ));
    assert!(matches!(
        b.receipt,
        PublicationEvidence::ReusedProjection { .. }
    ));
    let reservation_a = a.reserved_generation();
    let reservation_b = b.reserved_generation();
    assert_ne!(reservation_a, reservation_b);
    let receipt_a = a.receipt.receipt().clone();
    let ticket_a = a.stage().unwrap();
    assert!(matches!(
        session
            .attempt_public_manifest(b.stage().unwrap(), &PublicationDecision::new())
            .unwrap(),
        PublicationAttempt::Settled(PublicManifestCommit::Durable)
    ));

    let private_c = session.mint_detached_scope(ScopeId::ROOT).unwrap();
    let original_c =
        commit_certified_source(&mut session, private_c, "privateC :: Int\nprivateC = 33");
    let fact_revision = session.lib().log.publication_revision();
    let old_ceiling = session.lib().durable_allocation_ceiling().unwrap();
    session
        .lib_mut()
        .reserve_declaration_generations_durable(65)
        .unwrap();
    assert!(session.lib().durable_allocation_ceiling().unwrap() > old_ceiling);
    assert_eq!(session.lib().log.publication_revision(), fact_revision);
    let decision = PublicationDecision::new();
    let PublicationAttempt::RestageMetadata(certified) = session
        .attempt_public_manifest(ticket_a, &decision)
        .unwrap()
    else {
        panic!("unrelated owners, private facts and refill only change durable metadata")
    };
    assert_eq!(decision.phase(), PublicationPhase::Running);
    let PublicationAttempt::RestageMetadata(certified) =
        session.restage_publication_metadata(certified).unwrap()
    else {
        panic!("same semantic view must preserve certification")
    };
    let CertifiedPublication::Declarations(a) = &certified else {
        panic!("declaration certificate")
    };
    assert_eq!(a.reserved_generation(), reservation_a);
    assert!(Arc::ptr_eq(a.receipt.receipt(), &receipt_a));
    assert!(Arc::ptr_eq(a.intent(), &intent_a));
    assert_eq!(paired_version(&a.base.public), semantic_a);
    assert!(matches!(
        session
            .attempt_public_manifest(certified.stage().unwrap(), &decision)
            .unwrap(),
        PublicationAttempt::Settled(PublicManifestCommit::Durable)
    ));
    assert_eq!(decision.phase(), PublicationPhase::Published);
    let cold = recovery::load_domain_graph(
        Arc::new(RunRecoveryStore::open(recovery.store.path()).unwrap()),
        RecoveryDomainId::Root,
    )
    .unwrap();
    for generation in [
        original_a,
        original_b,
        original_c,
        reservation_a,
        reservation_b,
    ] {
        assert!(
            cold.node(generation).unwrap().is_some(),
            "durable fact {generation:?} must survive restaging and reopen"
        );
    }
    assert_eq!(
        cold.surface(&owner_a).unwrap().unwrap().declaration_root,
        Some(reservation_a)
    );
    assert_eq!(
        cold.surface(&owner_b).unwrap().unwrap().declaration_root,
        Some(reservation_b)
    );
    assert_eq!(cold.public_surfaces().count(), 2);
}

#[test]
fn semantic_change_between_metadata_retry_and_refresh_retains_execution_intent() {
    let root = tempfile::tempdir().unwrap();
    let mut lib = SessionLib::open(
        SessionId(9892),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
    let public = session.mint_scope(ScopeId::ROOT).unwrap();
    let owner = owner("root/refresh-race");
    session
        .initialize_durable_public_scope(owner.clone(), public)
        .unwrap();
    let admission = session
        .begin_durable_private_execution(&owner, public)
        .unwrap();
    let intent = session
        .freeze_execution_intent(&admission, vec![], vec![])
        .unwrap();
    let ExecutionPublication::Bindings(base) = session
        .restage_execution_publication(owner, intent.clone())
        .unwrap()
    else {
        panic!("bindings")
    };
    let ticket = base.stage().unwrap();
    session
        .lib_mut()
        .reserve_declaration_generations_durable(65)
        .unwrap();
    let decision = PublicationDecision::new();
    let PublicationAttempt::RestageMetadata(certified) =
        session.attempt_public_manifest(ticket, &decision).unwrap()
    else {
        panic!("refill restages")
    };
    session.advance_public_visibility(public);
    let PublicationAttempt::Rebase(retained) =
        session.restage_publication_metadata(certified).unwrap()
    else {
        panic!("relevant semantic change rebases")
    };
    assert!(Arc::ptr_eq(&retained, &intent));
    assert_eq!(decision.phase(), PublicationPhase::Running);
}

#[test]
fn declaration_rejection_baseline_refuses_retired_public_owner() {
    let root = tempfile::tempdir().unwrap();
    let lib = SessionLib::open(
        SessionId(9894),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
    let public = session.mint_scope(ScopeId::ROOT).unwrap();
    let private = session.mint_detached_scope(public).unwrap();
    let base = session
        .snapshot_publication_target(None, public, private, vec![], vec![])
        .unwrap();
    assert!(session.declaration_baseline_is_current(&base).unwrap());
    session.retire_scope(public);
    assert!(session.scope_tree().is_live(private));
    assert!(matches!(
        session.declaration_baseline_is_current(&base),
        Err(SessionError::StaleStagedDeclaration)
    ));
}

#[test]
fn library_replacement_under_same_runtime_owner_refuses_held_publication_and_intent() {
    let root = tempfile::tempdir().unwrap();
    let lib = SessionLib::open(
        SessionId(9895),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
    let public = session.mint_scope(ScopeId::ROOT).unwrap();
    let admission = session.begin_private_execution(public).unwrap();
    let intent = session
        .freeze_execution_intent(&admission, vec![], vec![])
        .unwrap();
    let ExecutionPublication::Bindings(base) = session
        .restage_ephemeral_execution_publication(intent.clone())
        .unwrap()
    else {
        panic!("bindings")
    };
    let before_first_freeze = session.begin_private_execution(public).unwrap();
    let certified = CertifiedPublication::Bindings(base.clone());
    let ticket = base.clone().stage().unwrap();
    let library = SessionLib::open(
        SessionId(9895),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    *session.lib_mut() = library;
    assert!(matches!(
        session.freeze_execution_intent(&before_first_freeze, vec![], vec![]),
        Err(SessionError::StaleStagedDeclaration)
    ));
    assert!(matches!(
        session.compile_view_for_execution(&before_first_freeze),
        Err(SessionError::StaleStagedDeclaration)
    ));
    assert!(matches!(
        session
            .attempt_public_manifest(ticket, &PublicationDecision::new())
            .unwrap(),
        PublicationAttempt::Settled(PublicManifestCommit::Refused { .. })
    ));
    assert!(matches!(
        session.restage_publication_metadata(certified).unwrap(),
        PublicationAttempt::Settled(PublicManifestCommit::Refused { .. })
    ));
    assert!(matches!(
        session.restage_ephemeral_execution_publication(intent),
        Err(SessionError::StaleStagedDeclaration)
    ));
    assert!(matches!(
        session.declaration_baseline_is_current(&base),
        Err(SessionError::StaleStagedDeclaration)
    ));
    let _removed_library = session.take_lib().unwrap();
    assert!(matches!(
        session.freeze_execution_intent(&before_first_freeze, vec![], vec![]),
        Err(SessionError::StaleStagedDeclaration)
    ));
    assert!(matches!(
        session.compile_view_for_execution(&before_first_freeze),
        Err(SessionError::StaleStagedDeclaration)
    ));
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config { cases: 32, ..proptest::test_runner::Config::default() })]
    #[test]
    fn bounded_interference_then_fair_drain_preserves_public_owners(
        interference in proptest::collection::vec(0u8..4, 0..12),
    ) {
        let root = tempfile::tempdir().unwrap();
        let mut lib = SessionLib::open(SessionId(9893), root.path(), ModuleEnv::standalone_default()).unwrap();
        let recovery = crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
        tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement)).unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let public_a = session.mint_scope(ScopeId::ROOT).unwrap();
        let public_b = session.mint_scope(ScopeId::ROOT).unwrap();
        let owner_a = owner("root/progress-a");
        let owner_b = owner("root/progress-b");
        session.initialize_durable_public_scope(owner_a.clone(), public_a).unwrap();
        session.initialize_durable_public_scope(owner_b.clone(), public_b).unwrap();
        let admission_a = session.begin_durable_private_execution(&owner_a, public_a).unwrap();
        let intent_a = session.freeze_execution_intent(&admission_a, vec![], vec![]).unwrap();
        let ExecutionPublication::Bindings(base) = session.restage_execution_publication(owner_a.clone(), intent_a.clone()).unwrap() else { panic!("bindings") };
        let mut ticket = Some(base.stage().unwrap());
        let decision = PublicationDecision::new();
        let mut model_owners = BTreeSet::from([owner_a.clone(), owner_b.clone()]);
        let mut last_generation = session.lib().generation();
        for action in interference {
            let revision = session.lib().log.publication_revision();
            match action {
                0 => { session.lib_mut().reserve_join_generation_durable().unwrap(); }
                1 => { session.lib_mut().reserve_declaration_generations_durable(65).unwrap(); }
                2 => {
                    let b = session.begin_durable_private_execution(&owner_b, public_b).unwrap();
                    let intent_b = session.freeze_execution_intent(&b, vec![], vec![]).unwrap();
                    let ExecutionPublication::Bindings(base) = session.restage_execution_publication(owner_b.clone(), intent_b).unwrap() else { panic!("bindings") };
                    proptest::prop_assert!(matches!(session.attempt_public_manifest(base.stage().unwrap(), &PublicationDecision::new()).unwrap(), PublicationAttempt::Settled(PublicManifestCommit::Durable)));
                    model_owners.insert(owner_b.clone());
                }
                3 => { session.advance_public_visibility(public_a); }
                _ => unreachable!(),
            }
            proptest::prop_assert_eq!(session.lib().log.publication_revision(), revision);
            proptest::prop_assert!(session.lib().generation() >= last_generation);
            last_generation = session.lib().generation();
            // The adversary stops before each next capture. No effects or declarations rerun.
            if action == 0 { continue; }
            ticket = Some(match session.attempt_public_manifest(ticket.take().unwrap(), &decision).unwrap() {
                PublicationAttempt::RestageMetadata(certified) => {
                    let PublicationAttempt::RestageMetadata(certified) = session.restage_publication_metadata(certified).unwrap() else { panic!("no new interference during refresh") };
                    certified.stage().unwrap()
                }
                PublicationAttempt::Rebase(retained) => {
                    proptest::prop_assert!(Arc::ptr_eq(&retained, &intent_a));
                    let ExecutionPublication::Bindings(base) = session.restage_execution_publication(owner_a.clone(), retained).unwrap() else { panic!("binding-only intent never reserves a join") };
                    base.stage().unwrap()
                }
                PublicationAttempt::Settled(PublicManifestCommit::Durable) => {
                    model_owners.insert(owner_a.clone());
                    // A fair drain already won; remaining operations are not execution retries.
                    break;
                }
                _ => panic!("bounded valid interference cannot terminate execution"),
            });
            proptest::prop_assert_eq!(decision.phase(), PublicationPhase::Running);
        }
        // Fair drain handles the last refill even if no attempt was made during
        // the interference phase. A refresh makes no compiler request or reservation.
        for _ in 0..3 {
            if decision.phase() == PublicationPhase::Published { break; }
            match session.attempt_public_manifest(ticket.take().unwrap(), &decision).unwrap() {
                PublicationAttempt::Settled(PublicManifestCommit::Durable) => { model_owners.insert(owner_a.clone()); }
                PublicationAttempt::RestageMetadata(certified) => {
                    let PublicationAttempt::RestageMetadata(certified) = session.restage_publication_metadata(certified).unwrap() else { panic!("fair refresh") };
                    ticket = Some(certified.stage().unwrap());
                }
                PublicationAttempt::Rebase(retained) => {
                    proptest::prop_assert!(Arc::ptr_eq(&retained, &intent_a));
                    let ExecutionPublication::Bindings(base) = session.restage_execution_publication(owner_a.clone(), retained).unwrap() else { panic!("binding-only fair rebase") };
                    ticket = Some(base.stage().unwrap());
                }
                _ => panic!("fair drain cannot terminate valid intent"),
            }
        }
        proptest::prop_assert_eq!(decision.phase(), PublicationPhase::Published);
        let cold = recovery::load_domain_graph(Arc::new(RunRecoveryStore::open(recovery.store.path()).unwrap()), RecoveryDomainId::Root).unwrap();
        let observed = cold.public_surfaces().map(|surface| surface.owner.clone()).collect::<BTreeSet<_>>();
        proptest::prop_assert_eq!(observed, model_owners);
        proptest::prop_assert_eq!(session.lib().generation(), last_generation);
    }
}
