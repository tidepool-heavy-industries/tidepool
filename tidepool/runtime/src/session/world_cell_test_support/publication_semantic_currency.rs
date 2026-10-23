//! Publication metadata currency through actual completed compiler Worlds.

use super::*;
use crate::session::paired_publication::semantic_currency_tests::{
    assert_preserved_certificate, assert_reused_projection, assert_stale_baseline,
};
use crate::session::paired_publication::ExecutionPublication;
use crate::session::{
    CertifiedDeclarationPublication, CertifiedPublication, ModuleEnv, PublicManifestCommit,
    PublicationAttempt, PublicationDecision, PublicationPhase, RecoveryDomainId,
    RecoveryPublicOwner, RunRecoveryStore, SessionError, SessionId, SessionLib,
};
use std::collections::BTreeSet;

#[derive(Clone)]
struct QuietOutput;
impl OutputSink for QuietOutput {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }
    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}
type TestSession = ResidentSession<frunk::HNil, QuietOutput>;
fn owner(name: &str) -> RecoveryPublicOwner {
    RecoveryPublicOwner::new(&tidepool_repr::ActorPath::parse(name).unwrap(), 1).unwrap()
}
fn captured_session(
    root: &std::path::Path,
    id: u64,
    durable: bool,
) -> (
    TestSession,
    InputRecipe,
    Option<crate::session::recovery_test_fixture::RecoveryFixture>,
) {
    tidepool_testing::eval_harness::require_extract();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let mut lib = SessionLib::open(SessionId(id), root, ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let recovery =
        durable.then(|| crate::session::recovery_test_fixture::RecoveryFixture::create(root));
    if let Some(recovery) = &recovery {
        tidepool_testing::with_settlement(|report| recovery.attach_root(&mut lib, report)).unwrap();
    }
    (
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib)),
        recipe,
        recovery,
    )
}
fn bootstrap_scope(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    owner: Option<&RecoveryPublicOwner>,
) -> Arc<crate::session::RuntimeLexicalScopeLease> {
    let source = resident.mint_notebook_scope_with_lease(None).unwrap();
    if let Some(owner) = owner {
        resident
            .initialize_durable_public_scope(owner.clone(), source.scope())
            .unwrap();
    }
    bootstrap_world_for(resident, recipe, &source);
    source
}
fn begin(
    resident: &mut TestSession,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
    owner: Option<&RecoveryPublicOwner>,
) -> Arc<crate::session::PrivateExecutionAdmission> {
    Arc::new(
        source_action_with_original(resident, source.original_source().clone(), |resident, _| {
            match owner {
                Some(owner) => resident.begin_durable_private_execution(owner, source.scope()),
                None => resident.begin_ephemeral_private_execution(source.scope()),
            }
        })
        .unwrap(),
    )
}
fn certify(
    base: crate::session::DeclarationPublicationBase,
    report: &mut dyn FnMut(crate::CompilerTransactionClose),
) -> crate::session::AcceptedDeclarationPublication {
    match base.certify(report).unwrap() {
        CertifiedDeclarationPublication::Accepted(accepted) => accepted,
        CertifiedDeclarationPublication::Rejected(_) => {
            panic!("actual checked declaration must certify")
        }
    }
}
fn publish_observation(
    resident: &mut TestSession,
    source: &Arc<crate::session::RuntimeLexicalScopeLease>,
    recipe: &InputRecipe,
    owner: &RecoveryPublicOwner,
) {
    let execution = begin(resident, source, Some(owner));
    execute_fixture_cell_for(resident, source, recipe, &execution, "()");
    let intent = resident.freeze_private_execution(&execution).unwrap();
    let ExecutionPublication::Bindings(base) = resident
        .state
        .restage_execution_publication(owner.clone(), intent)
        .unwrap()
    else {
        panic!("observation only")
    };
    assert!(matches!(
        resident
            .state
            .attempt_public_manifest(base.stage().unwrap(), &PublicationDecision::new())
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable
    ));
}

#[test]
fn reservations_refill_and_unrelated_private_facts_preserve_certificate_through_reopen() {
    let root = tempfile::tempdir().unwrap();
    let (mut resident, recipe, recovery) = captured_session(root.path(), 9891, true);
    let owner_a = owner("root/currency-a");
    let owner_b = owner("root/currency-b");
    let source_a = bootstrap_scope(&mut resident, &recipe, Some(&owner_a));
    let source_b = bootstrap_scope(&mut resident, &recipe, Some(&owner_b));
    let a = begin(&mut resident, &source_a, Some(&owner_a));
    let b = begin(&mut resident, &source_b, Some(&owner_b));
    let original_a = execute_fixture_cell_for(
        &mut resident,
        &source_a,
        &recipe,
        &a,
        "answerA :: Int\nanswerA = 11",
    )
    .0;
    let original_b = execute_fixture_cell_for(
        &mut resident,
        &source_b,
        &recipe,
        &b,
        "answerB :: Int\nanswerB = 22",
    )
    .0;
    let intent_a = resident.freeze_private_execution(&a).unwrap();
    let intent_b = resident.freeze_private_execution(&b).unwrap();
    let base_a = source_action_with_original(
        &mut resident,
        source_a.original_source().clone(),
        |resident, _| {
            resident
                .state
                .restage_declaration_publication(owner_a.clone(), intent_a.clone())
        },
    )
    .unwrap();
    let revision = resident.state.lib().log.publication_revision();
    let base_b = source_action_with_original(
        &mut resident,
        source_b.original_source().clone(),
        |resident, _| {
            resident
                .state
                .restage_declaration_publication(owner_b.clone(), intent_b)
        },
    )
    .unwrap();
    assert_eq!(resident.state.lib().log.publication_revision(), revision);
    let a = source_action_with_original(
        &mut resident,
        source_a.original_source().clone(),
        |_, report| certify(base_a, report),
    );
    let b = source_action_with_original(
        &mut resident,
        source_b.original_source().clone(),
        |_, report| certify(base_b, report),
    );
    assert_reused_projection(&a);
    assert_reused_projection(&b);
    let reservation_a = a.reserved_generation();
    let reservation_b = b.reserved_generation();
    assert_ne!(reservation_a, reservation_b);
    let mut private_original = None;
    let decision = PublicationDecision::new();
    let certified = assert_preserved_certificate(a, |a| {
        let ticket_a = a.stage().unwrap();
        assert!(matches!(
            resident
                .state
                .attempt_public_manifest(b.stage().unwrap(), &PublicationDecision::new())
                .unwrap(),
            PublicationAttempt::Settled(PublicManifestCommit::Durable)
        ));

        let c = begin(&mut resident, &source_b, Some(&owner_b));
        let original_c = execute_fixture_cell_for(
            &mut resident,
            &source_b,
            &recipe,
            &c,
            "privateC :: Int\nprivateC = 33",
        )
        .0;
        private_original = Some(original_c);
        let fact_revision = resident.state.lib().log.publication_revision();
        let old_ceiling = resident.state.lib().durable_allocation_ceiling().unwrap();
        resident
            .state
            .lib_mut()
            .reserve_declaration_generations_durable(65)
            .unwrap();
        assert!(resident.state.lib().durable_allocation_ceiling().unwrap() > old_ceiling);
        assert_eq!(
            resident.state.lib().log.publication_revision(),
            fact_revision
        );
        let compiler_requests = tidepool_extract_cmd::extract_spawn_count();
        let PublicationAttempt::RestageMetadata(certified) = resident
            .state
            .attempt_public_manifest(ticket_a, &decision)
            .unwrap()
        else {
            panic!("unrelated owners, private facts and refill only change durable metadata")
        };
        assert_eq!(decision.phase(), PublicationPhase::Running);
        let PublicationAttempt::RestageMetadata(certified) = resident
            .state
            .restage_publication_metadata(certified)
            .unwrap()
        else {
            panic!("same semantic view must preserve certification")
        };
        assert_eq!(
            tidepool_extract_cmd::extract_spawn_count(),
            compiler_requests
        );
        certified
    });
    let original_c = private_original.unwrap();
    let compiler_requests = tidepool_extract_cmd::extract_spawn_count();
    assert!(matches!(
        resident
            .state
            .attempt_public_manifest(certified.stage().unwrap(), &decision)
            .unwrap(),
        PublicationAttempt::Settled(PublicManifestCommit::Durable)
    ));
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        compiler_requests
    );
    assert_eq!(decision.phase(), PublicationPhase::Published);
    let cold = recovery::load_domain_graph(
        Arc::new(RunRecoveryStore::open(recovery.as_ref().unwrap().store.path()).unwrap()),
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
    let (mut resident, recipe, _recovery) = captured_session(root.path(), 9892, true);
    let owner = owner("root/refresh-race");
    let source = bootstrap_scope(&mut resident, &recipe, Some(&owner));
    let admission = begin(&mut resident, &source, Some(&owner));
    let intent = resident.freeze_private_execution(&admission).unwrap();
    let ExecutionPublication::Bindings(base) = resident
        .state
        .restage_execution_publication(owner.clone(), intent.clone())
        .unwrap()
    else {
        panic!("bindings")
    };
    let ticket = base.stage().unwrap();
    resident
        .state
        .lib_mut()
        .reserve_declaration_generations_durable(65)
        .unwrap();
    let decision = PublicationDecision::new();
    let PublicationAttempt::RestageMetadata(certified) = resident
        .state
        .attempt_public_manifest(ticket, &decision)
        .unwrap()
    else {
        panic!("refill restages")
    };
    publish_observation(&mut resident, &source, &recipe, &owner);
    let compiler_requests = tidepool_extract_cmd::extract_spawn_count();
    let PublicationAttempt::Rebase(retained) = resident
        .state
        .restage_publication_metadata(certified)
        .unwrap()
    else {
        panic!("relevant semantic change rebases")
    };
    assert!(Arc::ptr_eq(&retained, &intent));
    assert_eq!(decision.phase(), PublicationPhase::Running);
    assert_eq!(
        tidepool_extract_cmd::extract_spawn_count(),
        compiler_requests
    );
}

#[test]
fn raw_visibility_mutation_does_not_issue_a_current_compiler_world() {
    let root = tempfile::tempdir().unwrap();
    let (mut resident, recipe, _recovery) = captured_session(root.path(), 9896, false);
    let source = bootstrap_scope(&mut resident, &recipe, None);
    let execution = begin(&mut resident, &source, None);
    resident.freeze_private_execution(&execution).unwrap();
    resident.state.advance_public_visibility(source.scope());
    let refused = source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, _| resident.begin_ephemeral_private_execution(source.scope()),
    );
    assert!(
        matches!(refused, Err(SessionError::MissingPublishedCompilerWorld(scope)) if scope == source.scope())
    );
}

#[test]
fn declaration_rejection_baseline_refuses_retired_public_owner() {
    let root = tempfile::tempdir().unwrap();
    let (mut resident, recipe, _recovery) = captured_session(root.path(), 9894, false);
    let source = bootstrap_scope(&mut resident, &recipe, None);
    let public = source.scope();
    let admission = begin(&mut resident, &source, None);
    let private = admission.private_scope();
    let original = execute_fixture_cell_for(
        &mut resident,
        &source,
        &recipe,
        &admission,
        "retiredAnswer :: Int\nretiredAnswer = 1",
    )
    .0;
    let authored = resident
        .state
        .lib()
        .log
        .certified_authored_arc_at(original)
        .unwrap();
    let intent = resident.freeze_private_execution(&admission).unwrap();
    let accepted = source_action_with_original(
        &mut resident,
        source.original_source().clone(),
        |resident, report| {
            let ExecutionPublication::Declarations(base) = resident
                .state
                .restage_ephemeral_execution_publication(intent)
                .unwrap()
            else {
                panic!("actual declaration publication")
            };
            certify(base, report)
        },
    );
    let rejected =
        crate::session::paired_publication::tests::reject_missing_export(accepted, &authored);
    assert!(matches!(
        resident
            .revalidate_declaration_rejection(&rejected)
            .unwrap(),
        crate::session::DeclarationPublicationRejection::Rejected { .. }
    ));
    resident.retire_scope(public);
    assert!(resident.state.scope_tree().is_live(private));
    assert!(matches!(
        resident.revalidate_declaration_rejection(&rejected),
        Err(crate::session::SessionError::StaleStagedDeclaration)
    ));
}

#[test]
fn library_replacement_under_same_runtime_owner_refuses_held_publication_and_intent() {
    let root = tempfile::tempdir().unwrap();
    let (mut resident, recipe, _recovery) = captured_session(root.path(), 9895, false);
    let source = bootstrap_scope(&mut resident, &recipe, None);
    let admission = begin(&mut resident, &source, None);
    let intent = resident.freeze_private_execution(&admission).unwrap();
    let ExecutionPublication::Bindings(base) = resident
        .state
        .restage_ephemeral_execution_publication(intent.clone())
        .unwrap()
    else {
        panic!("bindings")
    };
    let before_first_freeze = begin(&mut resident, &source, None);
    let certified = CertifiedPublication::Bindings(base.clone());
    let ticket = base.clone().stage().unwrap();
    let library = SessionLib::open(
        SessionId(9895),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    *resident.state.lib_mut() = library;
    assert!(matches!(
        resident.freeze_private_execution(&before_first_freeze),
        Err(SessionError::TransitionRefused(
            crate::session::SessionTransitionReason::OwnerChanged
        ))
    ));
    assert!(matches!(
        resident
            .state
            .compile_view_for_execution(&before_first_freeze),
        Err(SessionError::StaleStagedDeclaration)
    ));
    assert!(matches!(
        resident
            .state
            .attempt_public_manifest(ticket, &PublicationDecision::new())
            .unwrap(),
        PublicationAttempt::Settled(PublicManifestCommit::Refused { .. })
    ));
    assert!(matches!(
        resident
            .state
            .restage_publication_metadata(certified)
            .unwrap(),
        PublicationAttempt::Settled(PublicManifestCommit::Refused { .. })
    ));
    assert!(matches!(
        resident
            .state
            .restage_ephemeral_execution_publication(intent),
        Err(SessionError::TransitionRefused(
            crate::session::SessionTransitionReason::OwnerChanged
        ))
    ));
    assert_stale_baseline(&resident.state, &base);
    let _removed_library = resident.state.take_lib().unwrap();
    assert!(matches!(
        resident.freeze_private_execution(&before_first_freeze),
        Err(SessionError::TransitionRefused(
            crate::session::SessionTransitionReason::OwnerChanged
        ))
    ));
    assert!(matches!(
        resident
            .state
            .compile_view_for_execution(&before_first_freeze),
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
        let (mut resident, recipe, recovery) = captured_session(root.path(), 9893, true);
        let owner_a = owner("root/progress-a");
        let owner_b = owner("root/progress-b");
        let source_a = bootstrap_scope(&mut resident, &recipe, Some(&owner_a));
        let source_b = bootstrap_scope(&mut resident, &recipe, Some(&owner_b));
        let admission_a = begin(&mut resident, &source_a, Some(&owner_a));
        let intent_a = resident.freeze_private_execution(&admission_a).unwrap();
        let ExecutionPublication::Bindings(base) = resident.state.restage_execution_publication(owner_a.clone(), intent_a.clone()).unwrap() else { panic!("bindings") };
        let mut ticket = Some(base.stage().unwrap());
        let decision = PublicationDecision::new();
        let mut model_owners = BTreeSet::from([owner_a.clone(), owner_b.clone()]);
        let mut last_generation = resident.state.lib().generation();
        for action in interference {
            let revision = resident.state.lib().log.publication_revision();
            match action {
                0 => { resident.state.lib_mut().reserve_join_generation_durable().unwrap(); }
                1 => { resident.state.lib_mut().reserve_declaration_generations_durable(65).unwrap(); }
                2 => {
                    let b = begin(&mut resident, &source_b, Some(&owner_b));
                    let intent_b = resident.freeze_private_execution(&b).unwrap();
                    let ExecutionPublication::Bindings(base) = resident.state.restage_execution_publication(owner_b.clone(), intent_b).unwrap() else { panic!("bindings") };
                    proptest::prop_assert!(matches!(resident.state.attempt_public_manifest(base.stage().unwrap(), &PublicationDecision::new()).unwrap(), PublicationAttempt::Settled(PublicManifestCommit::Durable)));
                    model_owners.insert(owner_b.clone());
                }
                3 => { publish_observation(&mut resident, &source_a, &recipe, &owner_a); }
                _ => unreachable!(),
            }
            proptest::prop_assert_eq!(resident.state.lib().log.publication_revision(), revision);
            proptest::prop_assert!(resident.state.lib().generation() >= last_generation);
            last_generation = resident.state.lib().generation();
            let compiler_requests = tidepool_extract_cmd::extract_spawn_count();
            // The adversary stops before each next capture. No effects or declarations rerun.
            if action == 0 { continue; }
            ticket = Some(match resident.state.attempt_public_manifest(ticket.take().unwrap(), &decision).unwrap() {
                PublicationAttempt::RestageMetadata(certified) => {
                    let PublicationAttempt::RestageMetadata(certified) = resident.state.restage_publication_metadata(certified).unwrap() else { panic!("no new interference during refresh") };
                    certified.stage().unwrap()
                }
                PublicationAttempt::Rebase(retained) => {
                    proptest::prop_assert!(Arc::ptr_eq(&retained, &intent_a));
                    let ExecutionPublication::Bindings(base) = resident.state.restage_execution_publication(owner_a.clone(), retained).unwrap() else { panic!("binding-only intent never reserves a join") };
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
            proptest::prop_assert_eq!(tidepool_extract_cmd::extract_spawn_count(), compiler_requests);
        }
        // Fair drain handles the last refill even if no attempt was made during
        // the interference phase. A refresh makes no compiler request or reservation.
        for _ in 0..3 {
            if decision.phase() == PublicationPhase::Published { break; }
            let compiler_requests = tidepool_extract_cmd::extract_spawn_count();
            match resident.state.attempt_public_manifest(ticket.take().unwrap(), &decision).unwrap() {
                PublicationAttempt::Settled(PublicManifestCommit::Durable) => { model_owners.insert(owner_a.clone()); }
                PublicationAttempt::RestageMetadata(certified) => {
                    let PublicationAttempt::RestageMetadata(certified) = resident.state.restage_publication_metadata(certified).unwrap() else { panic!("fair refresh") };
                    ticket = Some(certified.stage().unwrap());
                }
                PublicationAttempt::Rebase(retained) => {
                    proptest::prop_assert!(Arc::ptr_eq(&retained, &intent_a));
                    let ExecutionPublication::Bindings(base) = resident.state.restage_execution_publication(owner_a.clone(), retained).unwrap() else { panic!("binding-only fair rebase") };
                    ticket = Some(base.stage().unwrap());
                }
                _ => panic!("fair drain cannot terminate valid intent"),
            }
            proptest::prop_assert_eq!(tidepool_extract_cmd::extract_spawn_count(), compiler_requests);
        }
        proptest::prop_assert_eq!(decision.phase(), PublicationPhase::Published);
        let cold = recovery::load_domain_graph(Arc::new(RunRecoveryStore::open(recovery.as_ref().unwrap().store.path()).unwrap()), RecoveryDomainId::Root).unwrap();
        let observed = cold.public_surfaces().map(|surface| surface.owner.clone()).collect::<BTreeSet<_>>();
        proptest::prop_assert_eq!(observed, model_owners);
        proptest::prop_assert_eq!(resident.state.lib().generation(), last_generation);
    }
}
