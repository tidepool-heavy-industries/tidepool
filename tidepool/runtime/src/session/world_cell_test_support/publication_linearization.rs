//! Cancellation and actual native publication retain the winning public state.

use super::*;
use crate::session::paired_publication::ExecutionPublication;
use crate::session::{
    ModuleEnv, PersistentSession, PublicManifestCommit, PublicationCancellation,
    PublicationDecision, PublicationPhase, RecoveryPublicOwner, SessionId,
};
use std::sync::atomic::{AtomicBool, Ordering};
use tidepool_codegen::prepared_program::{PreparedOuter, PreparedResult};

use crate::session::recovery_test_fixture::RecoveryFixture;

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

#[derive(Clone, Copy)]
enum Persistence {
    Durable,
    Ephemeral,
}

#[derive(Clone, Copy)]
enum First {
    Cancellation,
    Publication,
}

fn actual_binding_publication_keeps_winner(persistence: Persistence, first: First) {
    let root = tempfile::tempdir().unwrap();
    let recovery =
        matches!(persistence, Persistence::Durable).then(|| RecoveryFixture::create(root.path()));
    tidepool_testing::eval_harness::require_extract();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let recipe = InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    );
    let mut lib = SessionLib::open(
        SessionId(4483),
        root.path().join("session"),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(recipe.include.clone());
    if let Some(recovery) = &recovery {
        tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
            .unwrap();
    }
    let mut resident =
        ResidentSession::unbootstrapped(frunk::HNil, QuietOutput, 1024 * 1024, Some(lib));
    let source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let public = source.scope();
    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/publication-linearization").unwrap(),
        1,
    )
    .unwrap();
    if matches!(persistence, Persistence::Durable) {
        assert!(matches!(
            resident
                .initialize_durable_public_scope(owner.clone(), public)
                .unwrap(),
            PublicManifestCommit::Durable
        ));
    }
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: public,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(&mut resident, &recipe, &source);
    let begin = |resident: &mut TestSession| {
        Arc::new(
            source_action_with_original(
                resident,
                source.original_source().clone(),
                |resident, _| match persistence {
                    Persistence::Durable => {
                        resident.begin_durable_private_execution(&owner, public)
                    }
                    Persistence::Ephemeral => resident.begin_ephemeral_private_execution(public),
                },
            )
            .unwrap(),
        )
    };
    let baseline = begin(&mut resident);
    execute_fixture_cell_for(
        &mut resident,
        &source,
        &recipe,
        &baseline,
        "let winner = (99 :: Int)\nlet kept = (100 :: Int)",
    );
    let baseline_intent = resident.freeze_private_execution(&baseline).unwrap();
    let old_id = resident
        .state
        .resolve_in(baseline.private_scope(), "winner")
        .unwrap()
        .id;
    let kept_id = resident
        .state
        .resolve_in(baseline.private_scope(), "kept")
        .unwrap()
        .id;
    let baseline_publication = match persistence {
        Persistence::Durable => resident
            .state
            .restage_execution_publication(owner.clone(), baseline_intent)
            .unwrap(),
        Persistence::Ephemeral => resident
            .state
            .restage_ephemeral_execution_publication(baseline_intent)
            .unwrap(),
    };
    let ExecutionPublication::Bindings(baseline_publication) = baseline_publication else {
        panic!("actual binding-only baseline")
    };
    assert!(matches!(
        resident
            .state
            .attempt_public_manifest(
                baseline_publication.stage().unwrap(),
                &PublicationDecision::new()
            )
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Durable | PublicManifestCommit::Ephemeral
    ));
    drop(baseline);
    resident.state.reap_admission_leases();
    let admission = begin(&mut resident);
    let stored = || {
        recovery.as_ref().map(|recovery| {
            (
                recovery.state(),
                recovery
                    .store
                    .read_public_owner(crate::session::RecoveryDomainId::Root, &owner)
                    .unwrap()
                    .unwrap(),
            )
        })
    };
    let private = admission.private_scope();
    execute_fixture_cell_for(
        &mut resident,
        &source,
        &recipe,
        &admission,
        "let winner = (101 :: Int)\nlet addition = (102 :: Int)",
    );
    let new_id = resident.state.resolve_in(private, "winner").unwrap().id;
    let addition_id = resident.state.resolve_in(private, "addition").unwrap().id;
    let intent = resident.freeze_private_execution(&admission).unwrap();
    let stage = |session: &mut PersistentSession| {
        let publication = match persistence {
            Persistence::Durable => session
                .restage_execution_publication(owner.clone(), intent.clone())
                .unwrap(),
            Persistence::Ephemeral => session
                .restage_ephemeral_execution_publication(intent.clone())
                .unwrap(),
        };
        let ExecutionPublication::Bindings(base) = publication else {
            panic!("native binding fixture requires no compiler join");
        };
        base.stage().unwrap()
    };
    let staged = stage(&mut resident.state);
    let before = resident
        .state
        .public_visibility_snapshot_in(public)
        .unwrap();
    let domain_before = stored();
    let compiler_requests = tidepool_extract_cmd::extract_spawn_count();
    let decision = PublicationDecision::new();
    let eligible = AtomicBool::new(true);

    match first {
        First::Cancellation => {
            assert_eq!(
                decision.request_cancellation_if(|| eligible.swap(false, Ordering::AcqRel)),
                Some(PublicationCancellation::RequestedBeforeCommit)
            );
            assert!(!eligible.load(Ordering::Acquire));
            assert!(matches!(
                resident
                    .state
                    .attempt_public_manifest(staged, &decision)
                    .map(crate::session::settled_manifest_attempt)
                    .unwrap(),
                PublicManifestCommit::Cancelled
            ));
            assert_eq!(
                resident
                    .state
                    .public_visibility_snapshot_in(public)
                    .unwrap(),
                before
            );
            assert_eq!(stored(), domain_before);
            assert_eq!(
                resident.state.resolve_in(public, "winner").unwrap().id,
                old_id
            );
            assert!(resident.state.resolve_in(public, "addition").is_none());
            assert_eq!(
                resident.state.resolve_in(private, "winner").unwrap().id,
                new_id
            );
            assert_eq!(decision.phase(), PublicationPhase::CancellationRequested);
            assert!(decision.terminate());
        }
        First::Publication => {
            assert!(matches!(
                (
                    persistence,
                    resident
                        .state
                        .attempt_public_manifest(staged, &decision)
                        .map(crate::session::settled_manifest_attempt)
                        .unwrap()
                ),
                (Persistence::Durable, PublicManifestCommit::Durable)
                    | (Persistence::Ephemeral, PublicManifestCommit::Ephemeral)
            ));
            assert_eq!(decision.phase(), PublicationPhase::Published);
            let published = resident
                .state
                .public_visibility_snapshot_in(public)
                .unwrap();
            assert_eq!(published.epoch, before.epoch + 1);
            assert!(published.bindings.contains(&("winner".into(), new_id)));
            assert!(published
                .bindings
                .contains(&("addition".into(), addition_id)));
            assert!(published.bindings.contains(&("kept".into(), kept_id)));
            let domain_published = stored();
            match persistence {
                Persistence::Durable => {
                    let before = domain_before.as_ref().unwrap();
                    let published = domain_published.as_ref().unwrap();
                    assert_ne!(published.0.content_revision, before.0.content_revision);
                    assert_eq!(published.0.snapshot_root, before.0.snapshot_root);
                    assert_ne!(published.1.surface_root, before.1.surface_root);
                    assert_eq!(published.1.epoch, before.1.epoch + 1);
                }
                Persistence::Ephemeral => assert_eq!(domain_published, None),
            }
            assert_eq!(
                decision.request_cancellation_if(|| eligible.swap(false, Ordering::AcqRel)),
                Some(PublicationCancellation::AlreadyPublished)
            );
            assert!(
                eligible.load(Ordering::Acquire),
                "a committed publication must not admit cancellation eligibility"
            );
            assert!(!decision.terminate());
            assert_eq!(decision.phase(), PublicationPhase::Published);
            assert_eq!(
                resident
                    .state
                    .public_visibility_snapshot_in(public)
                    .unwrap(),
                published
            );
            assert_eq!(stored(), domain_published);
        }
    }

    // Restaging the same completed intent is not permission to publish twice,
    // whether the original decision cancelled or committed its writes.
    let settled = resident
        .state
        .public_visibility_snapshot_in(public)
        .unwrap();
    let settled_domain = stored();
    let restaged = stage(&mut resident.state);
    assert!(matches!(
        resident
            .state
            .attempt_public_manifest(restaged, &decision)
            .map(crate::session::settled_manifest_attempt)
            .unwrap(),
        PublicManifestCommit::Cancelled
    ));
    assert_eq!(
        resident
            .state
            .public_visibility_snapshot_in(public)
            .unwrap(),
        settled
    );
    assert_eq!(stored(), settled_domain);

    drop(intent);
    drop(admission);
    resident.state.reap_admission_leases();
    assert!(!resident.state.scope_tree().is_live(private));
    resident
        .state
        .prepared_mut()
        .unwrap()
        .quiesce_and_collect_now()
        .unwrap();
    assert_eq!(
        resident.state.resolve_in(public, "kept").unwrap().id,
        kept_id
    );
    let expected = match first {
        First::Cancellation => {
            assert!(resident.state.bindings().get(new_id).is_none());
            assert!(resident.state.bindings().get(addition_id).is_none());
            old_id
        }
        First::Publication => new_id,
    };
    let winner = resident.state.resolve_in(public, "winner").unwrap();
    assert_eq!(winner.id, expected);
    let handle = winner.value.handle;
    let PreparedOuter::Constructor { fields, .. } = resident
        .state
        .prepared_mut()
        .unwrap()
        .inspect_retained(handle.raw())
        .expect("the winning public value survives private execution cleanup");
    let expected_value = match first {
        First::Cancellation => 99,
        First::Publication => 101,
    };
    assert!(
        matches!(fields.as_slice(), [PreparedResult::Scalar(value)] if *value == expected_value)
    );
    assert_eq!(
        resident
            .state
            .public_visibility_snapshot_in(public)
            .unwrap(),
        settled
    );
    assert_eq!(stored(), settled_domain);
    assert_eq!(tidepool_extract_cmd::extract_spawn_count(), compiler_requests);
}

#[test]
fn paired_guarded_cancellation_before_publication_preserves_public_winner() {
    for persistence in [Persistence::Durable, Persistence::Ephemeral] {
        actual_binding_publication_keeps_winner(persistence, First::Cancellation);
    }
}

#[test]
fn paired_publication_before_guarded_cancellation_preserves_committed_winner() {
    for persistence in [Persistence::Durable, Persistence::Ephemeral] {
        actual_binding_publication_keeps_winner(persistence, First::Publication);
    }
}
