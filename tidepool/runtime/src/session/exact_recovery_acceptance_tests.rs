use super::*;
use std::sync::Arc;
use tidepool_codegen::scope::ScopeId;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::ActorPath;
use tidepool_testing::effect_surface::TestEffectSurface;

fn owner(incarnation: u64) -> RecoveryPublicOwner {
    RecoveryPublicOwner::new(&ActorPath::parse("root/recovered").unwrap(), incarnation).unwrap()
}

struct TestRunOwner {
    root: PathBuf,
    _lock: std::fs::File,
}

#[derive(Clone)]
struct EmptyOutput;
impl OutputSink for EmptyOutput {
    fn drain(&self) -> Vec<String> {
        Vec::new()
    }
    fn snapshot(&self) -> Vec<String> {
        Vec::new()
    }
}
impl RecoveryRunAuthority for TestRunOwner {
    fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
        Ok(root.canonicalize()? == self.root)
    }
}

fn run_owner(root: &Path) -> Arc<TestRunOwner> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("test-run-owner.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    Arc::new(TestRunOwner {
        root: root.canonicalize().unwrap(),
        _lock: lock,
    })
}

struct TestSuccessor {
    run: Arc<TestRunOwner>,
    session: SessionId,
    target: ScopeId,
}
impl RecoverySuccessorAuthority for TestSuccessor {
    fn validate_successor(
        &self,
        root: &Path,
        old: &RecoveryPublicOwner,
        new: &RecoveryPublicOwner,
        session: SessionId,
        target: ScopeId,
    ) -> std::io::Result<bool> {
        Ok(self.run.owns_run(root)?
            && old == &owner(1)
            && new == &owner(2)
            && session == self.session
            && target == self.target)
    }
}

fn library(id: u64, source: &Path, manifest: &Path, include: &[PathBuf]) -> SessionLib {
    let mut lib = SessionLib::open(SessionId(id), source, ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(include.to_vec());
    lib.attach_owned_recovery_graph_v3(manifest, run_owner(manifest.parent().unwrap()))
        .unwrap();
    lib
}

#[test]
fn recovered_owner_validation_checks_the_current_owned_manifest_without_minting_scopes() {
    let durable = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let manifest = durable.path().join("declarations.json");
    let mut session =
        PersistentSession::new(Some(library(4410, source.path(), &manifest, &[])), 1024);
    let public = session.mint_isolated_scope();
    session
        .initialize_durable_public_scope(owner(1), public)
        .unwrap();
    let before_scope_count = session.scope_tree().len();
    let before_visibility = session.public_visibility_snapshot_in(public);
    let before_bytes = std::fs::read(&manifest).unwrap();
    assert!(session
        .lib()
        .validate_recovered_public_owner(&owner(1))
        .unwrap());
    assert!(!session.validate_recovered_public_owner(&owner(2)).unwrap());
    assert_eq!(session.scope_tree().len(), before_scope_count);
    assert_eq!(
        session.public_visibility_snapshot_in(public),
        before_visibility
    );
    assert_eq!(std::fs::read(&manifest).unwrap(), before_bytes);

    let mut newer = session
        .lib()
        .durable_graph
        .as_ref()
        .unwrap()
        .graph
        .wire_for_test();
    newer.high_water.0 += 1;
    newer.seal().unwrap();
    std::fs::write(&manifest, serde_json::to_vec(&newer).unwrap()).unwrap();
    assert!(session.validate_recovered_public_owner(&owner(1)).is_err());
    assert_eq!(session.scope_tree().len(), before_scope_count);
    std::fs::write(&manifest, b"corrupt manifest").unwrap();
    assert!(session.validate_recovered_public_owner(&owner(1)).is_err());
    std::fs::write(&manifest, before_bytes).unwrap();
    assert!(session.validate_recovered_public_owner(&owner(1)).unwrap());
}

#[test]
fn recovered_owner_validation_retains_and_revalidates_the_actual_run_authority() {
    struct RevocableRun {
        retained: Arc<TestRunOwner>,
        admitted: std::sync::atomic::AtomicBool,
    }
    impl RecoveryRunAuthority for RevocableRun {
        fn owns_run(&self, root: &Path) -> std::io::Result<bool> {
            Ok(self.admitted.load(std::sync::atomic::Ordering::Acquire)
                && self.retained.owns_run(root)?)
        }
    }
    let durable = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let manifest = durable.path().join("declarations.json");
    let retained = Arc::new(RevocableRun {
        retained: run_owner(durable.path()),
        admitted: std::sync::atomic::AtomicBool::new(true),
    });
    let mut lib = SessionLib::open(
        SessionId(4411),
        source.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    lib.attach_owned_recovery_graph_v3(&manifest, retained.clone())
        .unwrap();
    let mut session = PersistentSession::new(Some(lib), 1024);
    let public = session.mint_isolated_scope();
    session
        .initialize_durable_public_scope(owner(1), public)
        .unwrap();
    assert!(session.validate_recovered_public_owner(&owner(1)).unwrap());
    retained
        .admitted
        .store(false, std::sync::atomic::Ordering::Release);
    assert!(session.validate_recovered_public_owner(&owner(1)).is_err());
}

#[test]
fn recovered_public_owner_rejects_foreign_admission_and_incarnation_without_effects() {
    let durable = tempfile::tempdir().unwrap();
    let first_root = tempfile::tempdir().unwrap();
    let second_root = tempfile::tempdir().unwrap();
    let manifest = durable.path().join("declarations.json");
    let mut first =
        PersistentSession::new(Some(library(4401, first_root.path(), &manifest, &[])), 1024);
    let public = first.mint_isolated_scope();
    first.bind_durable_public_scope(owner(1), public).unwrap();
    let admission = first.begin_private_execution(public).unwrap();
    let intent = first
        .freeze_execution_intent(&admission, vec![], vec![])
        .unwrap();
    let ExecutionPublication::Bindings(base) = first
        .restage_execution_publication(owner(1), intent.clone())
        .unwrap()
    else {
        panic!("empty execution has a binding-only publication");
    };
    assert_eq!(
        first
            .publish_staged_public_manifest(base.stage().unwrap(), &PublicationDecision::new())
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let before = std::fs::read(&manifest).unwrap();
    drop(first);
    // Even identical serialized session/scope counters cannot restore this
    // process's admission owner. No heap bindings are fabricated on restart.
    let mut second = PersistentSession::new(
        Some(library(4401, second_root.path(), &manifest, &[])),
        1024,
    );
    assert!(second.recover_public_scope(&owner(2)).is_err());
    assert!(second.lib().durable_public_scopes.is_empty());
    let recovered = second.recover_public_scope(&owner(1)).unwrap();
    let snapshot = second.public_visibility_snapshot_in(recovered).unwrap();
    assert_eq!(snapshot.epoch, 1);
    assert!(snapshot.bindings.is_empty());
    assert!(snapshot.source_instances.is_empty());
    assert!(matches!(
        second.freeze_execution_intent(&admission, vec![], vec![]),
        Err(SessionError::StaleStagedDeclaration)
    ));
    assert!(matches!(
        second.restage_execution_publication(owner(1), intent),
        Err(SessionError::StaleStagedDeclaration)
    ));
    assert!(second.recover_public_scope(&owner(1)).is_err());
    assert_eq!(
        second.public_visibility_snapshot_in(recovered),
        Some(snapshot)
    );
    assert_eq!(std::fs::read(&manifest).unwrap(), before);
}

#[test]
fn materialization_retraction_attaches_authored_owner_before_publication() {
    tidepool_testing::eval_harness::require_extract();
    let durable = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let manifest = durable.path().join("declarations.json");
    let mut session = PersistentSession::new(
        Some(library(4412, source.path(), &manifest, &[])),
        1024 * 1024,
    );
    let public = session.mint_isolated_scope();
    session
        .initialize_durable_public_scope(owner(1), public)
        .unwrap();
    let admission = session.begin_private_execution(public).unwrap();
    let private = admission.private_scope();
    session
        .define_scoped_in(private, &[include_str!("fixtures/recovery-original.hs")])
        .unwrap();
    // A previously materialized value remains visible while this next binding
    // moves from the declaration environment into the binding store.
    let mut existing = prepared::tests::rooted_publication_fixture(&mut session, "existing", 4413);
    existing.scope = private;
    session.bind_in(private, existing).unwrap();
    let mut entry = prepared::tests::rooted_publication_fixture(&mut session, "answer", 4414);
    entry.scope = private;
    session.bind_replacing_decl_in(private, entry).unwrap();
    let generation = session.lib().scope_tip(private);
    let authored_owner = session
        .lib()
        .log
        .certified_authored_at(generation)
        .unwrap()
        .product()
        .owner()
        .clone();
    let selection = session
        .bindings()
        .source_domain_selection_in(session.scope_tree(), private)
        .unwrap();
    assert_eq!(
        selection
            .domain_for_owner(selection.current(), &authored_owner)
            .unwrap(),
        selection.current()
    );
    session
        .bindings()
        .prepare_authored_source_publication_in(
            session.scope_tree(),
            private,
            public,
            &[],
            &[],
            &[authored_owner],
            &[],
        )
        .unwrap();
    let before = session.public_visibility_snapshot_in(private).unwrap();
    session.retract_in(private, "absent").unwrap();
    assert_eq!(
        session.public_visibility_snapshot_in(private).unwrap(),
        before
    );
    let intent = session
        .freeze_execution_intent(&admission, vec![], vec![])
        .unwrap();
    let CertifiedDeclarationPublication::Accepted(accepted) = session
        .restage_declaration_publication(owner(1), intent)
        .unwrap()
        .certify()
        .unwrap()
    else {
        panic!("materialized declaration retraction must publish");
    };
    assert_eq!(
        session
            .publish_staged_public_manifest(accepted.stage().unwrap(), &PublicationDecision::new(),)
            .unwrap(),
        PublicManifestCommit::Durable
    );
}

#[test]
fn exact_publication_recovery_in_fresh_worker_preserves_originals_hidden_dependencies_and_retractions(
) {
    tidepool_testing::eval_harness::require_extract();
    assert!(
        std::env::var_os("TIDEPOOL_EXTRACT_DAEMON_SOCKET").is_none(),
        "this acceptance gate requires fresh compiler processes"
    );
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let durable = tempfile::tempdir().unwrap();
    let producer_root = tempfile::tempdir().unwrap();
    let manifest = durable.path().join("declarations.json");
    let mut producer = PersistentSession::new(
        Some(library(
            4402,
            producer_root.path(),
            &manifest,
            effects.include_paths(),
        )),
        1024 * 1024,
    );
    let public = producer.mint_isolated_scope();
    assert_eq!(
        producer
            .initialize_durable_public_scope(owner(1), public)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let admission = producer.begin_private_execution(public).unwrap();
    let original = producer
        .define_scoped_in(
            admission.private_scope(),
            &[include_str!("fixtures/recovery-original.hs")],
        )
        .unwrap();
    let dependent = producer
        .define_scoped_in(
            admission.private_scope(),
            &[include_str!("fixtures/recovery-dependent.hs")],
        )
        .unwrap();
    producer
        .retract_in(admission.private_scope(), "HiddenResult")
        .unwrap();
    producer
        .retract_in(admission.private_scope(), "answer")
        .unwrap();
    let intent = producer
        .freeze_execution_intent(&admission, vec![], vec![])
        .unwrap();
    let CertifiedDeclarationPublication::Accepted(accepted) = producer
        .restage_declaration_publication(owner(1), intent)
        .unwrap()
        .certify()
        .unwrap()
    else {
        panic!("real retained declarations must publish");
    };
    assert_eq!(
        producer
            .publish_staged_public_manifest(accepted.stage().unwrap(), &PublicationDecision::new())
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let expected = producer.public_visibility_snapshot_in(public).unwrap();
    let child_owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/exact-recovery/child").unwrap(),
        3,
    )
    .unwrap();
    let child = producer.mint_detached_scope(public).unwrap();
    let parent_surface = producer
        .lib()
        .durable_graph
        .as_ref()
        .unwrap()
        .graph
        .surface(&owner(1))
        .unwrap()
        .clone();
    assert_eq!(
        producer
            .initialize_durable_public_scope(child_owner.clone(), child)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let child_snapshot = producer.public_visibility_snapshot_in(child).unwrap();
    assert_eq!(child_snapshot.declaration_tip, expected.declaration_tip);
    assert_eq!(
        producer
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .graph
            .surface(&owner(1)),
        Some(&parent_surface)
    );
    let child_admission = producer
        .begin_durable_private_execution(&child_owner, child)
        .unwrap();
    assert_eq!(
        child_admission.admitted_public().declaration_tip,
        expected.declaration_tip
    );
    drop(child_admission);
    let uncertain_owner = RecoveryPublicOwner::new(
        &ActorPath::parse("root/exact-recovery/confirmed-child").unwrap(),
        4,
    )
    .unwrap();
    let uncertain_child = producer.mint_detached_scope(public).unwrap();
    producer.lib_mut().fail_recovery_durability_once = true;
    assert!(matches!(
        producer
            .initialize_durable_public_scope(uncertain_owner.clone(), uncertain_child)
            .unwrap(),
        PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
    ));
    let visible = std::fs::read(&manifest).unwrap();
    assert!(producer
        .begin_durable_private_execution(&uncertain_owner, uncertain_child)
        .is_err());
    assert!(producer
        .initialize_durable_public_scope(uncertain_owner.clone(), uncertain_child)
        .is_err());
    assert!(producer
        .confirm_durable_public_scope(&child_owner, uncertain_child)
        .is_err());
    assert!(producer
        .confirm_durable_public_scope(&uncertain_owner, child)
        .is_err());
    assert!(producer
        .validate_recovered_public_owner(&uncertain_owner)
        .is_err());
    producer
        .confirm_durable_public_scope(&uncertain_owner, uncertain_child)
        .unwrap();
    producer
        .confirm_durable_public_scope(&uncertain_owner, uncertain_child)
        .unwrap();
    assert_eq!(std::fs::read(&manifest).unwrap(), visible);
    assert_eq!(
        producer
            .initialize_durable_public_scope(uncertain_owner.clone(), uncertain_child)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert_eq!(std::fs::read(&manifest).unwrap(), visible);
    assert_eq!(
        producer
            .begin_durable_private_execution(&uncertain_owner, uncertain_child)
            .unwrap()
            .admitted_public()
            .declaration_tip,
        expected.declaration_tip
    );
    drop(admission);
    drop(producer);
    let removed_source = producer_root.path().to_owned();
    drop(producer_root);
    assert!(
        !removed_source.exists(),
        "producer source must be absent before recovery"
    );
    // No runtime owner, heap, source tree, or admission handle crosses the
    // OS-process boundary. These fixture expectations confer no authority;
    // the child reads and certifies the actual run-owned manifest itself.
    let spec = RecoveryChildSpec {
        manifest: manifest.clone(),
        original: original.0,
        dependent: dependent.0,
        epoch: expected.epoch,
        declaration_tip: expected.declaration_tip.0,
        result_file: durable.path().join("fresh-worker-result.json"),
    };
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "session::exact_recovery_acceptance_tests::exact_recovery_fresh_process_child",
            "--ignored",
            "--nocapture",
        ])
        .env(
            "TIDEPOOL_EXACT_RECOVERY_CHILD",
            serde_json::to_string(&spec).unwrap(),
        )
        .env("TIDEPOOL_EXTRACT_NO_DAEMON", "1")
        .env_remove("TIDEPOOL_EXTRACT_DAEMON_SOCKET")
        .stdin(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "fresh Rust recovery worker failed: {status}"
    );
    let child_pid: u32 = serde_json::from_slice(
        &std::fs::read(&spec.result_file)
            .expect("exact child phase must execute, not match zero tests"),
    )
    .unwrap();
    assert_ne!(
        child_pid,
        std::process::id(),
        "recovery must execute in a distinct Rust process"
    );
    let graph = recovery::read_v2(&manifest, durable.path())
        .unwrap()
        .unwrap()
        .graph;
    let successor = graph
        .surface(&owner(2))
        .expect("exact root successor surface");
    assert_eq!(successor.epoch, expected.epoch + 1);
    assert!(graph
        .public_surfaces()
        .all(|surface| surface.owner != owner(1)));
    for child in [child_owner, uncertain_owner] {
        let surface = graph
            .surface(&child)
            .expect("retained nonempty child surface");
        assert_eq!(surface.epoch, 1);
        assert_eq!(surface.declaration_root, Some(expected.declaration_tip));
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RecoveryChildSpec {
    manifest: PathBuf,
    original: u64,
    dependent: u64,
    epoch: u64,
    declaration_tip: u64,
    result_file: PathBuf,
}

#[test]
#[ignore = "invoked by the exact fresh-process parent with owned fixture paths"]
fn exact_recovery_fresh_process_child() {
    let spec = std::env::var("TIDEPOOL_EXACT_RECOVERY_CHILD")
        .expect("fresh-process parent must supply fixture expectations");
    execute_recovery_child(serde_json::from_str(&spec).unwrap());
}

fn execute_recovery_child(spec: RecoveryChildSpec) {
    tidepool_testing::eval_harness::require_extract();
    assert!(std::env::var_os("TIDEPOOL_EXTRACT_DAEMON_SOCKET").is_none());
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let manifest = spec.manifest;
    let durable = manifest.parent().unwrap();
    let before = std::fs::read(&manifest).unwrap();
    let original = Generation(spec.original);
    let dependent = Generation(spec.dependent);
    let consumer_root = tempfile::tempdir().unwrap();
    let recovery_owner = run_owner(durable);
    let mut recovered_library = SessionLib::open(
        SessionId(4403),
        consumer_root.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(effects.include_paths().to_vec());
    recovered_library
        .attach_owned_recovery_graph_v3(&manifest, recovery_owner.clone())
        .unwrap();
    let mut consumer = PersistentSession::new(Some(recovered_library), 1024 * 1024);
    let report = consumer.lib().declaration_recovery_report().unwrap();
    assert_eq!(report.successor_session, 4403);
    assert!(report
        .restored
        .iter()
        .any(|tip| tip.generation == spec.declaration_tip));
    assert!(!report.durability_unconfirmed);
    assert!(report
        .unavailable_bindings
        .iter()
        .all(|binding| binding.session != 4403));
    assert_eq!(consumer.lib().scope_tip(ScopeId::ROOT), Generation(0));
    // Ordinary minting creates an explicit G0 tip: successor transfer must
    // replace it with the recovered tip, not use inheritance-only seeding.
    let public = consumer.mint_scope(ScopeId::ROOT).unwrap();
    assert_eq!(std::fs::read(&manifest).unwrap(), before);
    let graph = recovery::read_v2(&manifest, durable)
        .unwrap()
        .unwrap()
        .graph;
    for alter_lexical in [false, true] {
        let mut altered = graph.wire_for_test();
        let original_node = graph.node(original).unwrap();
        let original_lexical = original_node.lexical.clone();
        let original_roots = original_node.lexical_roots.clone();
        let root = altered
            .nodes
            .iter_mut()
            .find(|node| node.id == Generation(spec.declaration_tip))
            .unwrap();
        if alter_lexical {
            // Same exact retained products, but an unauthorized wider lexical
            // selector/root set cannot replace the already owned byte read.
            let joined = root.lexical_roots[0].clone();
            root.lexical
                .iter_mut()
                .find(|node| node.owner == joined)
                .unwrap()
                .imports
                .extend(original_roots);
            for node in original_lexical {
                if !root
                    .lexical
                    .iter()
                    .any(|existing| existing.owner == node.owner)
                {
                    root.lexical.push(node);
                }
            }
        } else {
            root.exports[0].identity.occurrence = "ghostRecoveredExport".into();
        }
        altered.seal().unwrap();
        std::fs::write(&manifest, serde_json::to_vec_pretty(&altered).unwrap()).unwrap();
        assert!(consumer
            .transfer_recovered_public_owner(
                &owner(1),
                owner(2),
                public,
                Arc::new(TestSuccessor {
                    run: recovery_owner.clone(),
                    session: SessionId(4403),
                    target: public
                })
            )
            .is_err());
        assert!(consumer.lib().durable_public_scopes.is_empty());
        assert_eq!(consumer.lib().scope_tip(public), Generation(0));
        std::fs::write(&manifest, &before).unwrap();
    }
    assert_eq!(
        consumer
            .transfer_recovered_public_owner(
                &owner(1),
                owner(2),
                public,
                Arc::new(TestSuccessor {
                    run: recovery_owner,
                    session: SessionId(4403),
                    target: public
                })
            )
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert!(consumer.recover_public_scope(&owner(1)).is_err());
    let before = std::fs::read(&manifest).unwrap();
    let snapshot = consumer.public_visibility_snapshot_in(public).unwrap();
    assert_eq!(snapshot.declaration_tip, Generation(spec.declaration_tip));
    assert_eq!(snapshot.epoch, spec.epoch + 1);
    assert!(snapshot.bindings.is_empty());
    let mut recovered_scopes = vec![(public, "root/recovered")];
    for (path, incarnation) in [
        ("root/exact-recovery/child", 3),
        ("root/exact-recovery/confirmed-child", 4),
    ] {
        let child_owner =
            RecoveryPublicOwner::new(&ActorPath::parse(path).unwrap(), incarnation).unwrap();
        let child_scope = consumer.recover_public_scope(&child_owner).unwrap();
        let child_snapshot = consumer.public_visibility_snapshot_in(child_scope).unwrap();
        assert_eq!(
            child_snapshot.declaration_tip,
            Generation(spec.declaration_tip)
        );
        assert_eq!(child_snapshot.epoch, 1);
        assert!(child_snapshot.bindings.is_empty());
        assert_eq!(
            consumer
                .begin_durable_private_execution(&child_owner, child_scope)
                .unwrap()
                .admitted_public(),
            &child_snapshot
        );
        recovered_scopes.push((child_scope, path));
    }
    assert_eq!(std::fs::read(&manifest).unwrap(), before);
    let view = consumer.compile_view_in(public).unwrap();
    let context = view.exact_declaration_context().unwrap().clone();
    for generation in [original, dependent] {
        assert!(context
            .recovery_products()
            .iter()
            .any(|product| product.owner().module == SessionModule::lib(generation).module_name()));
        assert!(
            context
                .lexical_graph()
                .iter()
                .all(|node| node.owner.module != SessionModule::lib(generation).module_name()),
            "retained implementation must not become lexical authority"
        );
    }
    let read = recovery::read_v2(&manifest, durable).unwrap().unwrap();
    let projection = read.projection(&owner(2)).unwrap();
    let heads = projection
        .values()
        .filter_map(|head| match head {
            recovery::RecoveryHead::Available { export, .. } => Some(&export.identity),
            recovery::RecoveryHead::Tombstone(_) => None,
        })
        .collect::<Vec<_>>();
    assert!(heads.iter().any(|head| head.occurrence == "makeResult"
        && head.module == SessionModule::lib(original).module_name()));
    assert!(heads.iter().any(|head| head.occurrence == "recoveredAnswer"
        && head.module == SessionModule::lib(dependent).module_name()));
    assert!(heads
        .iter()
        .all(|head| head.occurrence != "HiddenResult" && head.occurrence != "answer"));
    let imports = view.turn_imports(&SourceImports::new());
    let include = view.include_paths(effects.include_paths());
    let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let injected = view.injected_module_names();
    let queries = [
        InspectionQuery::TypeOf("recoveredAnswer (41 :: Int)".into()),
        InspectionQuery::TypeOf("answer (41 :: Int)".into()),
        InspectionQuery::TypeOf("(undefined :: HiddenResult)".into()),
    ];
    let inspected = run_inspections(InspectionRequest {
        exact_context: Some(Arc::new(
            tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
        )),
        preamble: effects.preamble(),
        imports: &imports,
        include: &include,
        session_root: view.session_root(),
        inject_modules: &injected,
        queries: &queries,
        effects: Some(effects.row()),
    })
    .unwrap();
    assert!(matches!(&inspected[0], InspectionResult::Type { display, .. } if display == "Int"));
    assert!(matches!(&inspected[1], InspectionResult::Rejected { .. }));
    assert!(matches!(&inspected[2], InspectionResult::Rejected { .. }));
    let templates = resident_workbench_templates(effects.preamble(), effects.row(), &imports);
    let TurnResult::Expr { compiled, .. } = run_turn(TurnRequest {
        exact_context: Some(Arc::new(
            tidepool_toolchain::declaration_join::ExactCompileContext::new(context),
        )),
        session_id: Some(view.session()),
        turn_text: "recoveredAnswer (41 :: Int)",
        templates: &templates,
        include: &include,
        session_root: view.session_root(),
        inject_modules: &injected,
        gen: view.next_value_generation().0,
        verdict: None,
        target: None,
        retained_imports: &[],
    })
    .unwrap() else {
        panic!("recovered transitive call must compile as an expression");
    };
    assert_eq!(
        std::fs::read(&manifest).unwrap(),
        before,
        "recovery and fresh compilation cannot replay or publish original execution"
    );
    let mut resident =
        ResidentSession::from_persistent_for_test(frunk::HNil, EmptyOutput, consumer);
    for (scope, path) in recovered_scopes {
        let selected = resident.compile_view_in(scope).unwrap();
        assert_eq!(selected.turn_imports(&SourceImports::new()), imports);
        assert_eq!(
            selected
                .exact_declaration_context()
                .unwrap()
                .semantic_sha256(),
            view.exact_declaration_context().unwrap().semantic_sha256()
        );
        resident
            .set_actor_execution(
                SessionRunContext {
                    lexical_scope: scope,
                    ..SessionRunContext::ROOT
                },
                EffectRunPolicy::HandleOrSuspend,
                LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .unwrap();
        // All three scopes select the same exact immutable declaration root.
        // Reuse its compiled pure probe; mutable resident execution stays fresh.
        let ResidentOutcome::Completed { result, .. } = resident
            .run_with_sites("recovered_original_dependency", compiled.code())
            .unwrap()
        else {
            panic!("real recovered original dependency must execute in each selected scope");
        };
        assert_eq!(result.to_json(), serde_json::json!([42, "42"]));
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
        println!(
            "fresh recovery native execution: pid={} owner={path} scope={scope:?} result=42",
            std::process::id()
        );
    }
    let public_before = resident.public_visibility_snapshot_in(public).unwrap();
    let handles_before = resident.value_handle_count();
    let private;
    {
        use crate::session::turn::{compile_cell_program_admitted, consume_cell_program_item};
        use tidepool_toolchain::checked_cell::{CheckedCellSpecification, CheckedItemKind};

        // The standalone probe above covers immutable code reuse. Notebook cells
        // must also consume the recovered selection through whole-cell admission.
        let execution = Arc::new(resident.begin_private_execution(public).unwrap());
        private = execution.private_scope();
        let view = resident.compile_view_for_execution(&execution).unwrap();
        let imports = view.turn_imports(&SourceImports::new());
        let source = "let recoveredCellAnswer = recoveredAnswer (41 :: Int)";
        let specification = Arc::new(CheckedCellSpecification {
            admission_digest: [0; 32],
            cell_source: source.into(),
            template_source: resident_cell_check_template(
                effects.preamble(),
                effects.row(),
                &imports,
            ),
            turn_templates: resident_workbench_templates(
                effects.preamble(),
                effects.row(),
                &imports,
            )
            .iter()
            .map(|template| (template.kind.wire_name().into(), template.source.clone()))
            .collect(),
            injected_modules: view.injected_module_names(),
            reserved_declaration_modules: Vec::new(),
        });
        let include = view.include_paths(effects.include_paths());
        let plan = tidepool_toolchain::artifacts::parse_cell_plan(specification.clone(), &include)
            .unwrap();
        let admission = resident
            .admit_planned_cell_for_execution(
                execution,
                plan,
                specification.clone(),
                specification.specification_digest(),
                [1; 32],
                include,
                None,
            )
            .unwrap();
        let (checked, program) = compile_cell_program_admitted(admission.clone()).unwrap();
        assert_eq!(checked.items.len(), 1);
        let [item] = program.items() else {
            panic!("recovered cell must issue exactly one executable item");
        };
        let item = item.checked_item().clone();
        assert_eq!(item.kind(), CheckedItemKind::Bind);
        assert_eq!(item.binders(), ["recoveredCellAnswer"]);
        let prefix = resident
            .begin_cell_program(admission, program)
            .unwrap()
            .unwrap();
        let reservation = resident.admit_checked_item(prefix.clone(), item).unwrap();
        let TurnResult::Bind {
            bound, compiled, ..
        } = consume_cell_program_item(reservation.clone()).unwrap()
        else {
            panic!("recovered cell must consume its certified native bind");
        };
        let [binder] = bound.as_slice() else {
            panic!("recovered cell must retain its exact checked binder");
        };
        resident
            .set_run_context(SessionRunContext {
                lexical_scope: private,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        let outcome = resident
            .run_bind_with_sites(
                &binder.name,
                compiled.code(),
                binder,
                reservation.generation(),
            )
            .unwrap();
        assert!(
            matches!(
                outcome,
                ResidentOutcome::Completed { .. } | ResidentOutcome::BindingsCommitted { .. }
            ),
            "recovered whole-cell bind did not complete: {outcome:?}"
        );
        let (id, ..) = resident.current_binding_in(private, &binder.name).unwrap();
        assert_eq!(id, SessionVarId::from_extract(binder.var_id));
        let custody = resident
            .retain_binding_custody_in(private, &binder.name, id)
            .unwrap()
            .unwrap();
        assert_eq!(
            resident.render_retained_preview(&custody, 64),
            Some("42".into())
        );
        assert_eq!(
            prefix.snapshot().compiler_prefix().next_item(),
            checked.items.len()
        );
        assert_eq!(
            resident.public_visibility_snapshot_in(public).unwrap(),
            public_before
        );
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
    }
    resident.retire_scope(private);
    assert_eq!(resident.value_handle_count(), handles_before);
    assert_eq!(
        resident.public_visibility_snapshot_in(public).unwrap(),
        public_before
    );
    assert_eq!(std::fs::read(&manifest).unwrap(), before);
    std::fs::write(
        &spec.result_file,
        serde_json::to_vec(&std::process::id()).unwrap(),
    )
    .unwrap();
}

fn persist_empty_public(root: &Path, source: &Path, id: u64) -> PathBuf {
    let manifest = root.join("declarations.json");
    let mut session = PersistentSession::new(Some(library(id, source, &manifest, &[])), 1024);
    let public = session.mint_isolated_scope();
    assert_eq!(
        session
            .initialize_durable_public_scope(owner(1), public)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    manifest
}

#[test]
fn owned_manifest_read_refuses_foreign_run_symlink_and_changed_public_selectors() {
    let durable = tempfile::tempdir().unwrap();
    let original_source = tempfile::tempdir().unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let manifest = persist_empty_public(durable.path(), original_source.path(), 4410);
    let bytes = std::fs::read(&manifest).unwrap();
    let mut unowned = SessionLib::open(
        SessionId(4411),
        source.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    assert!(unowned.attach_recovery_graph_v2(&manifest).is_err());
    assert_eq!(unowned.log.generation(), Generation(0));
    assert!(unowned
        .attach_owned_recovery_graph_v3(&manifest, run_owner(foreign.path()))
        .is_err());
    let alias = foreign.path().join("escaped.json");
    std::os::unix::fs::symlink(&manifest, &alias).unwrap();
    assert!(unowned
        .attach_owned_recovery_graph_v3(&alias, run_owner(foreign.path()))
        .is_err());
    let mut session =
        PersistentSession::new(Some(library(4412, source.path(), &manifest, &[])), 1024);
    let mut altered = recovery::read_v2(&manifest, durable.path())
        .unwrap()
        .unwrap()
        .graph
        .wire_for_test();
    altered.public_surfaces[0].owner = owner(2);
    altered.seal().unwrap();
    // The same retained products with copied, edited public selectors cannot
    // change the immutable owner-issued read admitted by this runtime.
    std::fs::write(&manifest, serde_json::to_vec_pretty(&altered).unwrap()).unwrap();
    assert!(session.recover_public_scope(&owner(1)).is_err());
    assert!(session.recover_public_scope(&owner(2)).is_err());
    assert!(session.lib().durable_public_scopes.is_empty());
    assert_ne!(std::fs::read(&manifest).unwrap(), bytes);
}

#[test]
fn successor_transfer_fences_old_admissions_after_durable_and_uncertain_rename() {
    for uncertain in [false, true] {
        let durable = tempfile::tempdir().unwrap();
        let original_source = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let manifest = persist_empty_public(durable.path(), original_source.path(), 4420);
        let run = run_owner(durable.path());
        let mut lib = SessionLib::open(
            SessionId(4421),
            source.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        lib.attach_owned_recovery_graph_v3(&manifest, run.clone())
            .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024);
        let target = session.mint_scope(ScopeId::ROOT).unwrap();
        let old = session.begin_private_execution(target).unwrap();
        let old_intent = session
            .freeze_execution_intent(&old, vec![], vec![])
            .unwrap();
        session.lib_mut().fail_recovery_durability_once = uncertain;
        let outcome = session
            .transfer_recovered_public_owner(
                &owner(1),
                owner(2),
                target,
                Arc::new(TestSuccessor {
                    run,
                    session: SessionId(4421),
                    target,
                }),
            )
            .unwrap();
        if uncertain {
            assert!(matches!(
                outcome,
                PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
            ));
        } else {
            assert_eq!(outcome, PublicManifestCommit::Durable);
        }
        let bytes = std::fs::read(&manifest).unwrap();
        assert!(matches!(
            session.freeze_execution_intent(&old, vec![], vec![]),
            Err(SessionError::StaleStagedDeclaration)
        ));
        assert!(matches!(
            session.restage_execution_publication(owner(2), old_intent),
            Err(SessionError::StaleStagedDeclaration)
        ));
        assert!(session.recover_public_scope(&owner(1)).is_err());
        assert_eq!(
            session.public_visibility_snapshot_in(target).unwrap().epoch,
            2
        );
        assert_eq!(std::fs::read(&manifest).unwrap(), bytes);
    }
}

#[test]
fn successor_transfer_retains_only_the_sealed_live_bootstrap_dependencies() {
    for uncertain in [false, true] {
        let durable = tempfile::tempdir().unwrap();
        let original_source = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let manifest = persist_empty_public(durable.path(), original_source.path(), 4440);
        let run = run_owner(durable.path());
        let mut lib = SessionLib::open(
            SessionId(4441),
            source.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        lib.attach_owned_recovery_graph_v3(&manifest, run.clone())
            .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let target = session.mint_isolated_scope();
        let (_, keys) = prepared::tests::install_source_publication_fixture(&mut session, target);
        assert_eq!(keys.len(), 2);
        let snapshot = session.public_visibility_snapshot_in(target).unwrap();
        assert_eq!(snapshot.declaration_tip, Generation(0));
        assert!(snapshot.bindings.is_empty());
        let before = std::fs::read(&manifest).unwrap();
        let transfer = |session: &mut PersistentSession, scope| {
            session.transfer_recovered_public_owner(
                &owner(1),
                owner(2),
                scope,
                Arc::new(TestSuccessor {
                    run: run.clone(),
                    session: SessionId(4441),
                    target: scope,
                }),
            )
        };
        assert!(matches!(
            transfer(&mut session, target),
            Err(SessionError::InvalidRecoveryInitialization {
                reason: RecoveryInitializationFailure::MissingSeal,
                ..
            })
        ));
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
        session.seal_recovery_initialization_scope(target).unwrap();
        session.seal_recovery_initialization_scope(target).unwrap();
        let foreign = session.mint_isolated_scope();
        assert!(matches!(
            transfer(&mut session, foreign),
            Err(SessionError::InvalidRecoveryInitialization {
                reason: RecoveryInitializationFailure::ForeignSeal,
                ..
            })
        ));
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
        session.lib_mut().fail_recovery_durability_once = uncertain;
        let outcome = transfer(&mut session, target).unwrap();
        if uncertain {
            assert!(matches!(
                outcome,
                PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
            ));
        } else {
            assert_eq!(outcome, PublicManifestCommit::Durable);
        }
        let transferred = session.public_visibility_snapshot_in(target).unwrap();
        assert_eq!(transferred.source_instances, snapshot.source_instances);
        assert_eq!(
            transferred.machine_incarnation,
            snapshot.machine_incarnation
        );
        assert!(transferred.bindings.is_empty());
        assert_eq!(transferred.epoch, 2);
        let bytes = std::fs::read(&manifest).unwrap();
        let persisted = recovery::read_v2(&manifest, durable.path())
            .unwrap()
            .unwrap();
        // Current-machine bootstrap handles are retained locally. They never
        // become reminted persisted source instances during owner transfer.
        assert!(persisted
            .graph
            .surface(&owner(1))
            .unwrap()
            .source_instances
            .is_empty());
        session
            .confirm_durable_public_scope(&owner(2), target)
            .unwrap();
        session
            .confirm_durable_public_scope(&owner(2), target)
            .unwrap();
        assert_eq!(std::fs::read(&manifest).unwrap(), bytes);
        for lease in session
            .bindings()
            .source_instances_in(session.scope_tree(), target)
        {
            assert_eq!(
                session
                    .prepared()
                    .unwrap()
                    .prepared_handle_of(lease.handle().raw()),
                Some(lease.handle())
            );
        }
    }
}

#[test]
fn successor_initialization_refuses_changed_or_released_native_dependencies() {
    for released in [false, true] {
        let durable = tempfile::tempdir().unwrap();
        let original_source = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let manifest = persist_empty_public(durable.path(), original_source.path(), 4442);
        let run = run_owner(durable.path());
        let mut lib = SessionLib::open(
            SessionId(4443),
            source.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        lib.attach_owned_recovery_graph_v3(&manifest, run.clone())
            .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let target = session.mint_isolated_scope();
        let (_, keys) = prepared::tests::install_source_publication_fixture(&mut session, target);
        session.seal_recovery_initialization_scope(target).unwrap();
        let reason = if released {
            let lease = session
                .bindings()
                .source_instances_in(session.scope_tree(), target)[0]
                .clone();
            assert!(session.prepared_mut().unwrap().release(lease.handle()));
            RecoveryInitializationFailure::UnavailableNativeDependency
        } else {
            assert!(session.retire_fixture_source_subset(target, &keys[..1]));
            RecoveryInitializationFailure::ChangedNativeDependencies
        };
        let before = std::fs::read(&manifest).unwrap();
        assert!(matches!(
            session.transfer_recovered_public_owner(
                &owner(1), owner(2), target,
                Arc::new(TestSuccessor { run, session: SessionId(4443), target }),
            ),
            Err(SessionError::InvalidRecoveryInitialization { reason: actual, .. }) if actual == reason
        ));
        assert_eq!(std::fs::read(&manifest).unwrap(), before);
        assert!(session.lib().durable_public_scopes.is_empty());
        assert_eq!(
            session.public_visibility_snapshot_in(target).unwrap().epoch,
            0
        );
    }
}

#[test]
fn initial_public_owner_survives_restart_before_first_cell() {
    let durable = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let manifest = persist_empty_public(durable.path(), source.path(), 4430);
    let read = recovery::read_v2(&manifest, durable.path())
        .unwrap()
        .unwrap();
    assert!(read.graph.nodes().next().is_none());
    assert_eq!(read.graph.high_water(), Generation(0));
    assert_eq!(read.graph.surface(&owner(1)).unwrap().owner, owner(1));
    let next_source = tempfile::tempdir().unwrap();
    let run = run_owner(durable.path());
    let mut lib = SessionLib::open(
        SessionId(4431),
        next_source.path(),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    lib.attach_owned_recovery_graph_v3(&manifest, run.clone())
        .unwrap();
    let mut session = PersistentSession::new(Some(lib), 1024);
    let target = session.mint_isolated_scope();
    assert_eq!(
        session
            .transfer_recovered_public_owner(
                &owner(1),
                owner(2),
                target,
                Arc::new(TestSuccessor {
                    run,
                    session: SessionId(4431),
                    target
                })
            )
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let read = recovery::read_v2(&manifest, durable.path())
        .unwrap()
        .unwrap();
    assert!(read.graph.nodes().next().is_none());
    assert_eq!(read.graph.high_water(), Generation(0));
    assert_eq!(read.graph.surface(&owner(2)).unwrap().owner, owner(2));
    assert_eq!(
        session
            .public_visibility_snapshot_in(target)
            .unwrap()
            .declaration_tip,
        Generation(0)
    );
}

#[test]
fn durable_child_initializer_preserves_parent_and_refuses_a_local_only_owner() {
    let durable = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let manifest = durable.path().join("declarations.json");
    let mut session =
        PersistentSession::new(Some(library(4407, source.path(), &manifest, &[])), 1024);
    let parent = session.mint_isolated_scope();
    assert_eq!(
        session
            .initialize_durable_public_scope(owner(1), parent)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let before_parent = session.public_visibility_snapshot_in(parent).unwrap();
    let parent_surface = session
        .lib()
        .durable_graph
        .as_ref()
        .unwrap()
        .graph
        .surface(&owner(1))
        .unwrap()
        .clone();
    let child = session.mint_detached_scope(parent).unwrap();
    let child_owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/exact-recovery/child").unwrap(),
        3,
    )
    .unwrap();
    session
        .bind_durable_public_scope(child_owner.clone(), child)
        .unwrap();
    assert!(session
        .begin_durable_private_execution(&child_owner, child)
        .is_err());
    let before = std::fs::read(&manifest).unwrap();
    assert!(session
        .initialize_durable_public_scope(child_owner.clone(), parent)
        .is_err());
    assert_eq!(std::fs::read(&manifest).unwrap(), before);
    assert_eq!(
        session
            .initialize_durable_public_scope(child_owner.clone(), child)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert_eq!(
        session.public_visibility_snapshot_in(parent),
        Some(before_parent)
    );
    assert_eq!(
        session
            .lib()
            .durable_graph
            .as_ref()
            .unwrap()
            .graph
            .surface(&owner(1)),
        Some(&parent_surface)
    );
    assert!(session.lib().tips.contains_key(&child));
    assert_eq!(
        session
            .begin_durable_private_execution(&child_owner, child)
            .unwrap()
            .admitted_public()
            .epoch,
        1
    );
    let initialized = std::fs::read(&manifest).unwrap();
    assert_eq!(
        session
            .initialize_durable_public_scope(child_owner.clone(), child)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert_eq!(std::fs::read(&manifest).unwrap(), initialized);
}

#[test]
fn durable_child_visible_uncertainty_confirms_without_reinitializing_or_reexecuting() {
    let durable = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let manifest = durable.path().join("declarations.json");
    let mut session =
        PersistentSession::new(Some(library(4408, source.path(), &manifest, &[])), 1024);
    let parent = session.mint_isolated_scope();
    session
        .initialize_durable_public_scope(owner(1), parent)
        .unwrap();
    let child = session.mint_detached_scope(parent).unwrap();
    let child_owner = RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/exact-recovery/child").unwrap(),
        3,
    )
    .unwrap();
    session.lib_mut().fail_recovery_durability_once = true;
    assert!(matches!(
        session
            .initialize_durable_public_scope(child_owner.clone(), child)
            .unwrap(),
        PublicManifestCommit::PublishedDurabilityUnconfirmed { .. }
    ));
    let visible = std::fs::read(&manifest).unwrap();
    assert!(session
        .begin_durable_private_execution(&child_owner, child)
        .is_err());
    assert!(session
        .initialize_durable_public_scope(child_owner.clone(), child)
        .is_err());
    assert!(session
        .confirm_durable_public_scope(&owner(99), child)
        .is_err());
    assert!(session
        .confirm_durable_public_scope(&child_owner, parent)
        .is_err());
    assert!(session
        .begin_durable_private_execution(&child_owner, child)
        .is_err());
    assert!(session
        .validate_recovered_public_owner(&child_owner)
        .is_err());
    session
        .confirm_durable_public_scope(&child_owner, child)
        .unwrap();
    session
        .confirm_durable_public_scope(&child_owner, child)
        .unwrap();
    assert!(session
        .validate_recovered_public_owner(&child_owner)
        .unwrap());
    assert_eq!(
        session
            .initialize_durable_public_scope(child_owner.clone(), child)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert_eq!(std::fs::read(&manifest).unwrap(), visible);
    assert_eq!(
        session
            .begin_durable_private_execution(&child_owner, child)
            .unwrap()
            .admitted_public()
            .epoch,
        1
    );
}
