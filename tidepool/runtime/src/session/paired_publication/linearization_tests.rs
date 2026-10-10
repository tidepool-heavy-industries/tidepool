//! Cancellation and actual native publication retain the winning public state.

use super::*;
use crate::session::{
    ModuleEnv, PublicManifestCommit, PublicationCancellation, PublicationDecision,
    PublicationPhase, RecoveryRunAuthority, SessionId,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tidepool_codegen::prepared_program::{PreparedOuter, PreparedResult};

struct RunOwner {
    root: PathBuf,
    _lock: std::fs::File,
}

impl RecoveryRunAuthority for RunOwner {
    fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
        Ok(root.canonicalize()? == self.root)
    }
}

fn run_owner(root: &Path) -> Arc<RunOwner> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    Arc::new(RunOwner {
        root: root.canonicalize().unwrap(),
        _lock: lock,
    })
}

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
    let manifest = root.path().join("declarations.json");
    let mut lib = SessionLib::open(
        SessionId(4483),
        root.path().join("session"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    if matches!(persistence, Persistence::Durable) {
        tidepool_testing::with_settlement(|settlement| {
            lib.attach_owned_recovery_graph_v3(&manifest, run_owner(root.path()), settlement)
        })
        .unwrap();
    }
    let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
    let public = session.mint_isolated_scope();
    let old_winner =
        crate::session::prepared::tests::evaluated_publication_fixture(&mut session, "winner", 710);
    let old_id = old_winner.id;
    session.bind_in(public, old_winner).unwrap();
    let kept =
        crate::session::prepared::tests::evaluated_publication_fixture(&mut session, "kept", 711);
    let kept_id = kept.id;
    session.bind_in(public, kept).unwrap();

    let owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/publication-linearization").unwrap(),
        1,
    )
    .unwrap();
    let admission = match persistence {
        Persistence::Durable => {
            assert_eq!(
                session
                    .initialize_durable_public_scope(owner.clone(), public)
                    .unwrap(),
                PublicManifestCommit::Durable
            );
            session
                .begin_durable_private_execution(&owner, public)
                .unwrap()
        }
        Persistence::Ephemeral => session.begin_ephemeral_private_execution(public).unwrap(),
    };
    let private = admission.private_scope();
    let new_winner =
        crate::session::prepared::tests::evaluated_publication_fixture(&mut session, "winner", 712);
    let new_id = new_winner.id;
    session.bind_in(private, new_winner).unwrap();
    let private_only = crate::session::prepared::tests::evaluated_publication_fixture(
        &mut session,
        "addition",
        713,
    );
    let addition_id = private_only.id;
    session.bind_in(private, private_only).unwrap();
    let intent = session
        .freeze_execution_intent(&admission, vec![new_id, addition_id], vec![])
        .unwrap();
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
    let staged = stage(&mut session);
    let before = session.public_visibility_snapshot_in(public).unwrap();
    let manifest_before = std::fs::read(&manifest).ok();
    let decision = PublicationDecision::new();
    let eligible = AtomicBool::new(true);

    match first {
        First::Cancellation => {
            assert_eq!(
                decision.request_cancellation_if(|| eligible.swap(false, Ordering::AcqRel)),
                Some(PublicationCancellation::RequestedBeforeCommit)
            );
            assert!(!eligible.load(Ordering::Acquire));
            assert_eq!(
                session
                    .publish_staged_public_manifest(staged, &decision)
                    .unwrap(),
                PublicManifestCommit::Cancelled
            );
            assert_eq!(
                session.public_visibility_snapshot_in(public).unwrap(),
                before
            );
            assert_eq!(std::fs::read(&manifest).ok(), manifest_before);
            assert_eq!(session.resolve_in(public, "winner").unwrap().id, old_id);
            assert!(session.resolve_in(public, "addition").is_none());
            assert_eq!(session.resolve_in(private, "winner").unwrap().id, new_id);
            assert_eq!(decision.phase(), PublicationPhase::CancellationRequested);
            assert!(decision.terminate());
        }
        First::Publication => {
            assert_eq!(
                session
                    .publish_staged_public_manifest(staged, &decision)
                    .unwrap(),
                match persistence {
                    Persistence::Durable => PublicManifestCommit::Durable,
                    Persistence::Ephemeral => PublicManifestCommit::Ephemeral,
                }
            );
            assert_eq!(decision.phase(), PublicationPhase::Published);
            let published = session.public_visibility_snapshot_in(public).unwrap();
            assert_eq!(published.epoch, before.epoch + 1);
            assert!(published.bindings.contains(&("winner".into(), new_id)));
            assert!(published
                .bindings
                .contains(&("addition".into(), addition_id)));
            assert!(published.bindings.contains(&("kept".into(), kept_id)));
            let manifest_published = std::fs::read(&manifest).ok();
            match persistence {
                Persistence::Durable => assert_ne!(manifest_published, manifest_before),
                Persistence::Ephemeral => assert_eq!(manifest_published, None),
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
                session.public_visibility_snapshot_in(public).unwrap(),
                published
            );
            assert_eq!(std::fs::read(&manifest).ok(), manifest_published);
        }
    }

    // Restaging the same completed intent is not permission to publish twice,
    // whether the original decision cancelled or committed its writes.
    let settled = session.public_visibility_snapshot_in(public).unwrap();
    let settled_bytes = std::fs::read(&manifest).ok();
    let restaged = stage(&mut session);
    assert_eq!(
        session
            .publish_staged_public_manifest(restaged, &decision)
            .unwrap(),
        PublicManifestCommit::Cancelled
    );
    assert_eq!(
        session.public_visibility_snapshot_in(public).unwrap(),
        settled
    );
    assert_eq!(std::fs::read(&manifest).ok(), settled_bytes);

    drop(intent);
    drop(admission);
    session.reap_admission_leases();
    assert!(!session.scope_tree().is_live(private));
    session
        .prepared_mut()
        .unwrap()
        .quiesce_and_collect_now()
        .unwrap();
    assert_eq!(session.resolve_in(public, "kept").unwrap().id, kept_id);
    let expected = match first {
        First::Cancellation => {
            assert!(session.bindings().get(new_id).is_none());
            assert!(session.bindings().get(addition_id).is_none());
            old_id
        }
        First::Publication => new_id,
    };
    let winner = session.resolve_in(public, "winner").unwrap();
    assert_eq!(winner.id, expected);
    let handle = winner.value.handle;
    let PreparedOuter::Constructor { identity, fields } = session
        .prepared_mut()
        .unwrap()
        .inspect_retained(handle.raw())
        .expect("the winning public value survives private execution cleanup");
    assert_eq!(identity, tidepool_repr::DataConId(980));
    assert!(matches!(fields.as_slice(), [PreparedResult::Scalar(99)]));
    assert_eq!(
        session.public_visibility_snapshot_in(public).unwrap(),
        settled
    );
    assert_eq!(std::fs::read(&manifest).ok(), settled_bytes);
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

#[test]
fn fixed_sibling_intents_restage_after_observation_and_manifest_changes_without_reexecution() {
    let root = tempfile::tempdir().unwrap();
    let mut lib = SessionLib::open(
        SessionId(4484),
        root.path().join("session"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    tidepool_testing::with_settlement(|settlement| {
        lib.attach_owned_recovery_graph_v3(
            &root.path().join("declarations.json"),
            run_owner(root.path()),
            settlement,
        )
    })
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
    let intent_a = session
        .freeze_execution_intent(&a, vec![id_a], vec![])
        .unwrap();
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
    // result. Its observation invalidates caches, and its manifest makes A's
    // staged graph stale; neither event changes A's private semantic view.
    let value_b = crate::session::prepared::tests::evaluated_publication_fixture(
        &mut session,
        "observationB",
        721,
    );
    let id_b = value_b.id;
    session.bind_in(b.private_scope(), value_b).unwrap();
    session.save_observation(id_b, &[]);
    let intent_b = session
        .freeze_execution_intent(&b, vec![id_b], vec![])
        .unwrap();
    let ExecutionPublication::Bindings(base_b) = session
        .restage_execution_publication(owner_b, intent_b)
        .unwrap()
    else {
        panic!("binding-only publication");
    };
    assert_eq!(
        session
            .publish_staged_public_manifest(base_b.stage().unwrap(), &PublicationDecision::new())
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert_eq!(
        session
            .public_visibility_snapshot_in(a.private_scope())
            .unwrap(),
        before_a
    );
    let decision_a = PublicationDecision::new();
    assert_eq!(
        session
            .publish_staged_public_manifest(staged_a, &decision_a)
            .unwrap(),
        PublicManifestCommit::Stale
    );
    assert_eq!(decision_a.phase(), PublicationPhase::Running);
    let ExecutionPublication::Bindings(retry_a) = session
        .restage_execution_publication(owner_a, intent_a.clone())
        .unwrap()
    else {
        panic!("same binding-only intent");
    };
    assert_eq!(
        session
            .publish_staged_public_manifest(retry_a.stage().unwrap(), &decision_a)
            .unwrap(),
        PublicManifestCommit::Durable
    );
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
        &session
            .freeze_execution_intent(&a, vec![id_a], vec![])
            .unwrap()
    ));
}
