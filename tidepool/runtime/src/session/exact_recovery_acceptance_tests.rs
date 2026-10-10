use super::*;
use std::sync::Arc;
use tidepool_codegen::scope::ScopeId;
use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
use tidepool_repr::ActorPath;
use tidepool_testing::effect_surface::TestEffectSurface;

fn owner(incarnation: u64) -> RecoveryPublicOwner {
    RecoveryPublicOwner::new(&ActorPath::parse("root/recovered").unwrap(), incarnation).unwrap()
}

use super::recovery_test_fixture::{domain_state, graph, RecoveryFixture};
pub(super) use super::recovery_test_fixture::{run_owner, TestRunOwner};

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

fn library(id: u64, recovery: &RecoveryFixture, include: &[PathBuf]) -> SessionLib {
    let artifact_root = recovery.store.path().parent().unwrap().join("compiler");
    let mut lib = SessionLib::open(
        SessionId(id),
        &artifact_root,
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(include.to_vec());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    lib
}

fn adopt_recovery_declaration(
    session: &mut PersistentSession,
    execution: Arc<PrivateExecutionAdmission>,
    effects: &TestEffectSurface,
    source: &str,
) -> Generation {
    use crate::session::turn::compile_cell_program_admitted;
    use tidepool_toolchain::checked_cell::{CheckedCellSpecification, CheckedItemKind};

    let view = session.compile_view_for_execution(&execution).unwrap();
    let imports = view.turn_imports(&SourceImports::new());
    let specification = Arc::new(CheckedCellSpecification {
        admission_digest: [0; 32],
        cell_source: source.into(),
        template_source: resident_cell_check_template(effects.preamble(), effects.row(), &imports),
        turn_templates: resident_workbench_templates(effects.preamble(), effects.row(), &imports)
            .iter()
            .map(|template| {
                let kind = match template.kind {
                    TemplateSelector::Decl => "decl",
                    TemplateSelector::Bind => "bind",
                    TemplateSelector::BindDiscard => "binddiscard",
                    TemplateSelector::Expr => "expr",
                };
                (kind.into(), template.source.clone())
            })
            .collect(),
        injected_modules: view.injected_module_names(),
        reserved_declaration_modules: Vec::new(),
    });
    let include = view.include_paths(effects.include_paths());
    let plan = tidepool_testing::with_settlement(|settlement| {
        tidepool_toolchain::artifacts::parse_cell_plan(specification.clone(), &include, settlement)
    })
    .unwrap();
    let admission = session
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
    let (_, program) = tidepool_testing::with_settlement(|settlement| {
        compile_cell_program_admitted(admission.clone(), settlement)
    })
    .unwrap();
    let [item] = program.items() else {
        panic!("recovery fixture must issue one original declaration");
    };
    let item = item.checked_item().clone();
    assert_eq!(item.kind(), CheckedItemKind::Declaration);
    let prefix = session
        .begin_cell_program(admission, program)
        .unwrap()
        .unwrap();
    let reservation = session.admit_checked_item(prefix.clone(), item).unwrap();
    let commit = session.adopt_checked_declaration(reservation).unwrap();
    assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 1);
    commit.generation
}

#[test]
fn recovered_owner_validation_checks_the_current_domain_without_minting_scopes() {
    let durable = tempfile::tempdir().unwrap();
    let recovery = RecoveryFixture::create(durable.path());
    let mut session = PersistentSession::new(Some(library(4410, &recovery, &[])), 1024);
    let public = session.mint_isolated_scope();
    session
        .initialize_durable_public_scope(owner(1), public)
        .unwrap();
    let before_scope_count = session.scope_tree().len();
    let before_visibility = session.public_visibility_snapshot_in(public);
    let before_domain = domain_state(session.lib());
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
    assert_eq!(domain_state(session.lib()), before_domain);

    let mut sibling = library(4412, &recovery, &[]);
    sibling.reserve_join_generation_durable().unwrap();
    assert!(session.validate_recovered_public_owner(&owner(1)).is_err());
    assert_eq!(session.scope_tree().len(), before_scope_count);
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
    let recovery = RecoveryFixture::create(durable.path());
    let retained = Arc::new(RevocableRun {
        retained: recovery.owner.clone(),
        admitted: std::sync::atomic::AtomicBool::new(true),
    });
    let mut lib = SessionLib::open(
        SessionId(4411),
        recovery.store.path().parent().unwrap().join("compiler"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    tidepool_testing::with_settlement(|settlement| {
        lib.attach_owned_recovery_domain(
            recovery.store.clone(),
            super::recovery_store::RecoveryDomainId::Root,
            lib.include_dir().canonicalize().unwrap(),
            retained.clone(),
            settlement,
        )
    })
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
    let recovery = RecoveryFixture::create(durable.path());
    let mut first = PersistentSession::new(Some(library(4401, &recovery, &[])), 1024);
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
    let before = recovery.state();
    drop(first);
    // Even identical serialized session/scope counters cannot restore this
    // process's admission owner. No heap bindings are fabricated on restart.
    let mut second = PersistentSession::new(Some(library(4401, &recovery, &[])), 1024);
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
    assert_eq!(recovery.state(), before);
}

#[test]
fn public_scoped_declarations_reuse_selected_originals_without_lexical_names() {
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let durable = tempfile::tempdir().unwrap();
    let recovery = RecoveryFixture::create(durable.path());
    let mut session = PersistentSession::new(
        Some(library(4420, &recovery, effects.include_paths())),
        1024 * 1024,
    );
    let public = session.mint_isolated_scope();
    session
        .initialize_durable_public_scope(owner(1), public)
        .unwrap();
    let execution = session.begin_private_execution(public).unwrap();
    let private = execution.private_scope();
    let public_before = session.public_visibility_snapshot_in(public).unwrap();
    let original = tidepool_testing::with_settlement(|settlement| {
        session.define_scoped_in(
            private,
            &[include_str!("fixtures/recovery-original.hs")],
            settlement,
        )
    })
    .expect("public declaration API certifies its first original");
    let original_owner = session
        .lib()
        .log
        .certified_authored_at(original)
        .unwrap()
        .product()
        .owner()
        .clone();
    let selected = session.compile_view_in(private).unwrap();
    assert!(selected
        .exact_declaration_context()
        .unwrap()
        .lexical_graph()
        .iter()
        .all(|node| node.owner.module != original_owner.module));
    let dependent = tidepool_testing::with_settlement(|settlement| {
        session.define_scoped_in(
            private,
            &[include_str!("fixtures/recovery-dependent.hs")],
            settlement,
        )
    })
    .expect("public declaration API reuses the selected original behind its lexical join");
    let certificate = session.lib().log.certified_authored_at(dependent).unwrap();
    assert!(certificate
        .recovery_products()
        .iter()
        .any(|product| product.owner() == &original_owner));
    let selected = session.compile_view_in(private).unwrap();
    let context = selected.exact_declaration_context().unwrap();
    let original_root = context.authored_native_root(original.0).unwrap();
    let dependent_root = context.authored_native_root(dependent.0).unwrap();
    assert!(certificate
        .artifact_view()
        .dependencies()
        .iter()
        .any(|(from, to, edge)| {
            *from == dependent_root
                && *to == original_root
                && matches!(
                    edge,
                    tidepool_toolchain::artifact_inventory::ArtifactDependency::NativeGroup { .. }
                )
        }));

    let private_before = session.public_visibility_snapshot_in(private).unwrap();
    let high_water = session.lib().generation();
    let rejected = tidepool_testing::with_settlement(|settlement| {
        session.define_scoped_in(private, &["bad :: Int\nbad = True"], settlement)
    })
    .expect_err("ill-typed public declaration is refused during validation");
    assert!(matches!(rejected, SessionError::ValidationFailed(_)));
    assert_eq!(session.lib().scope_tip(private), dependent);
    assert!(session.lib().generation() > high_water);
    assert_eq!(
        session.public_visibility_snapshot_in(private),
        Some(private_before)
    );
    assert!(session
        .lib()
        .current_decl_heads_in(private)
        .iter()
        .all(|(name, _)| name != "bad"));
    assert_eq!(
        session.public_visibility_snapshot_in(public),
        Some(public_before)
    );

    tidepool_testing::with_settlement(|settlement| {
        session.retract_many_in(
            private,
            &["answer".into(), "HiddenResult".into()],
            settlement,
        )
    })
    .unwrap();
    let view = session.compile_view_in(private).unwrap();
    let context = view.exact_declaration_context().unwrap().clone();
    assert!(context
        .recovery_products()
        .iter()
        .any(|product| product.owner() == &original_owner));
    let imports = view.turn_imports(&SourceImports::new());
    let include = view.include_paths(effects.include_paths());
    let include = include.iter().map(PathBuf::as_path).collect::<Vec<_>>();
    let injected = view.injected_module_names();
    let queries = [
        InspectionQuery::TypeOf("recoveredAnswer (41 :: Int)".into()),
        InspectionQuery::TypeOf("answer (41 :: Int)".into()),
        InspectionQuery::TypeOf("(undefined :: HiddenResult)".into()),
    ];
    let inspected = tidepool_testing::with_settlement(|settlement| {
        run_inspections(
            InspectionRequest {
                exact_context: Some(Arc::new(
                    tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
                )),
                preamble: effects.preamble(),
                imports: &imports,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                queries: &queries,
                effects: Some(&crate::session::HaskellTypeSource::from(effects.row())),
            },
            settlement,
        )
    })
    .unwrap();
    assert!(matches!(&inspected[0], InspectionResult::Type { display, .. } if display == "Int"));
    assert!(matches!(&inspected[1], InspectionResult::Rejected { .. }));
    assert!(matches!(&inspected[2], InspectionResult::Rejected { .. }));
    let templates = vec![TurnTemplate {
        kind: TemplateSelector::Expr,
        source: assemble_expression_module(
            &insert_preamble_imports(effects.preamble(), &imports),
            "__result",
            effects.row(),
            "{{TURN}}",
            ExpressionLift::Pure,
        ),
    }];
    let TurnResult::Expr { compiled, .. } = tidepool_testing::with_settlement(|settlement| {
        run_turn(
            TurnRequest {
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
            },
            settlement,
        )
    })
    .unwrap() else {
        panic!("dependent declaration must compile as a native expression");
    };
    let mut resident = ResidentSession::from_persistent_for_test(frunk::HNil, EmptyOutput, session);
    resident
        .set_actor_execution(
            SessionRunContext {
                lexical_scope: private,
                ..SessionRunContext::ROOT
            },
            EffectRunPolicy::HandleOrSuspend,
            LivePayloadPolicy::HASKELL_EFFECT_VALUE,
        )
        .unwrap();
    let ResidentOutcome::Completed { result, .. } =
        tidepool_testing::with_settlement(|settlement| {
            resident.run_with_sites(
                "public_scoped_original_dependency",
                compiled.code(),
                settlement,
            )
        })
        .unwrap()
    else {
        panic!("dependent declaration must execute its retained original");
    };
    assert_eq!(result.to_json(), serde_json::json!(42));
}

#[test]
fn materialization_retraction_attaches_authored_owner_before_publication() {
    tidepool_testing::eval_harness::require_extract();
    let durable = tempfile::tempdir().unwrap();
    let recovery = RecoveryFixture::create(durable.path());
    let mut session = PersistentSession::new(Some(library(4412, &recovery, &[])), 1024 * 1024);
    let public = session.mint_isolated_scope();
    session
        .initialize_durable_public_scope(owner(1), public)
        .unwrap();
    let admission = session.begin_private_execution(public).unwrap();
    let private = admission.private_scope();
    tidepool_testing::with_settlement(|settlement| {
        session.define_scoped_in(
            private,
            &[include_str!("fixtures/recovery-original.hs")],
            settlement,
        )
    })
    .unwrap();
    // A previously materialized value remains visible while this next binding
    // moves from the declaration environment into the binding store.
    let mut existing = prepared::tests::rooted_publication_fixture(&mut session, "existing", 4413);
    existing.scope = private;
    session.bind_in(private, existing).unwrap();
    let mut entry = prepared::tests::rooted_publication_fixture(&mut session, "answer", 4414);
    entry.scope = private;
    tidepool_testing::with_settlement(|settlement| {
        session.bind_replacing_decl_in(private, entry, settlement)
    })
    .unwrap();
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
    tidepool_testing::with_settlement(|settlement| {
        session.retract_in(private, "absent", settlement)
    })
    .unwrap();
    assert_eq!(
        session.public_visibility_snapshot_in(private).unwrap(),
        before
    );
    let intent = session
        .freeze_execution_intent(&admission, vec![], vec![])
        .unwrap();
    let CertifiedDeclarationPublication::Accepted(accepted) =
        tidepool_testing::with_settlement(|settlement| {
            session
                .restage_declaration_publication(owner(1), intent)
                .unwrap()
                .certify(settlement)
        })
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
    let recovery = RecoveryFixture::create(durable.path());
    let mut producer = PersistentSession::new(
        Some(library(4402, &recovery, effects.include_paths())),
        1024 * 1024,
    );
    let public = producer.mint_isolated_scope();
    assert_eq!(
        producer
            .initialize_durable_public_scope(owner(1), public)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let admission = Arc::new(producer.begin_private_execution(public).unwrap());
    let original = adopt_recovery_declaration(
        &mut producer,
        admission.clone(),
        &effects,
        include_str!("fixtures/recovery-original.hs"),
    );
    let dependent = adopt_recovery_declaration(
        &mut producer,
        admission.clone(),
        &effects,
        include_str!("fixtures/recovery-dependent.hs"),
    );
    tidepool_testing::with_settlement(|settlement| {
        producer.retract_in(admission.private_scope(), "HiddenResult", settlement)
    })
    .unwrap();
    tidepool_testing::with_settlement(|settlement| {
        producer.retract_in(admission.private_scope(), "answer", settlement)
    })
    .unwrap();
    let intent = producer
        .freeze_execution_intent(&admission, vec![], vec![])
        .unwrap();
    let CertifiedDeclarationPublication::Accepted(accepted) =
        tidepool_testing::with_settlement(|settlement| {
            producer
                .restage_declaration_publication(owner(1), intent)
                .unwrap()
                .certify(settlement)
        })
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
    let parent_surface = recovery
        .store
        .read_public_owner(RecoveryDomainId::Root, &owner(1))
        .unwrap()
        .unwrap();
    assert_eq!(
        producer
            .initialize_durable_public_scope(child_owner.clone(), child)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let child_snapshot = producer.public_visibility_snapshot_in(child).unwrap();
    assert_eq!(child_snapshot.declaration_tip, expected.declaration_tip);
    assert_eq!(
        recovery
            .store
            .read_public_owner(RecoveryDomainId::Root, &owner(1))
            .unwrap(),
        Some(parent_surface)
    );
    let child_admission = producer
        .begin_durable_private_execution(&child_owner, child)
        .unwrap();
    assert_eq!(
        child_admission.admitted_public().declaration_tip,
        expected.declaration_tip
    );
    drop(child_admission);
    let additional_owner = RecoveryPublicOwner::new(
        &ActorPath::parse("root/exact-recovery/second-child").unwrap(),
        4,
    )
    .unwrap();
    let additional_child = producer.mint_detached_scope(public).unwrap();
    assert_eq!(
        producer
            .initialize_durable_public_scope(additional_owner.clone(), additional_child)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert_eq!(
        producer
            .begin_durable_private_execution(&additional_owner, additional_child)
            .unwrap()
            .admitted_public()
            .declaration_tip,
        expected.declaration_tip
    );
    drop(admission);
    drop(producer);
    let removed_source = durable.path().join("compiler/Tidepool/Session");
    assert!(
        removed_source.is_dir(),
        "the issuer actually wrote authored source modules"
    );
    assert!(durable.path().join("compiler/artifacts").is_dir());
    std::fs::remove_dir_all(&removed_source).unwrap();
    assert!(
        !removed_source.exists(),
        "producer authored sources must be absent before recovery"
    );
    // Only fixture expectations cross the process boundary; the new process
    // acquires the run lock and authenticates the retained compiler artifacts.
    let spec = RecoveryChildSpec {
        store_path: recovery.store.path().to_owned(),
        original: original.0,
        dependent: dependent.0,
        epoch: expected.epoch,
        declaration_tip: expected.declaration_tip.0,
        result_file: durable.path().join("fresh-worker-result.json"),
    };
    drop(recovery);
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
    let reopened = RecoveryFixture::reopen(run_owner(durable.path()));
    let successor = reopened
        .store
        .read_public_owner(super::recovery_store::RecoveryDomainId::Root, &owner(2))
        .unwrap()
        .unwrap();
    assert_eq!(successor.epoch, expected.epoch + 1);
    assert!(reopened
        .store
        .read_public_owner(super::recovery_store::RecoveryDomainId::Root, &owner(1))
        .unwrap()
        .is_none());
    for child in [child_owner, additional_owner] {
        let surface = reopened
            .store
            .read_public_owner(super::recovery_store::RecoveryDomainId::Root, &child)
            .unwrap()
            .unwrap();
        assert_eq!(surface.epoch, 1);
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct RecoveryChildSpec {
    store_path: PathBuf,
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
    let durable = spec.store_path.parent().unwrap();
    let recovery = RecoveryFixture::reopen(run_owner(durable));
    assert_eq!(recovery.store.path(), spec.store_path);
    let before = recovery.state();
    let original = Generation(spec.original);
    let dependent = Generation(spec.dependent);
    let recovery_owner = recovery.owner.clone();
    let recovered_library = library(4403, &recovery, effects.include_paths());
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
    assert_same_content(&recovery, &before);
    let other = consumer.mint_isolated_scope();
    for (session, target) in [(SessionId(4404), public), (SessionId(4403), other)] {
        assert!(consumer
            .transfer_recovered_public_owner(
                &owner(1),
                owner(2),
                public,
                Arc::new(TestSuccessor {
                    run: recovery_owner.clone(),
                    session,
                    target
                }),
            )
            .is_err());
        assert!(consumer.lib().durable_public_scopes.is_empty());
        assert_eq!(consumer.lib().scope_tip(public), Generation(0));
        assert_same_content(&recovery, &before);
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
    let before = recovery.state();
    let snapshot = consumer.public_visibility_snapshot_in(public).unwrap();
    assert_eq!(snapshot.declaration_tip, Generation(spec.declaration_tip));
    assert_eq!(snapshot.epoch, spec.epoch + 1);
    assert!(snapshot.bindings.is_empty());
    let mut recovered_scopes = vec![(public, "root/recovered")];
    for (path, incarnation) in [
        ("root/exact-recovery/child", 3),
        ("root/exact-recovery/second-child", 4),
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
    assert_same_content(&recovery, &before);
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
    let exports = consumer
        .lib()
        .log
        .certified_exports_at(Generation(spec.declaration_tip))
        .unwrap();
    let heads = exports
        .iter()
        .map(|export| &export.head)
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
    let inspected = tidepool_testing::with_settlement(|settlement| {
        run_inspections(
            InspectionRequest {
                exact_context: Some(Arc::new(
                    tidepool_toolchain::declaration_join::ExactCompileContext::new(context.clone()),
                )),
                preamble: effects.preamble(),
                imports: &imports,
                include: &include,
                session_root: view.session_root(),
                inject_modules: &injected,
                queries: &queries,
                effects: Some(&crate::session::HaskellTypeSource::from(effects.row())),
            },
            settlement,
        )
    })
    .unwrap();
    assert!(matches!(&inspected[0], InspectionResult::Type { display, .. } if display == "Int"));
    assert!(matches!(&inspected[1], InspectionResult::Rejected { .. }));
    assert!(matches!(&inspected[2], InspectionResult::Rejected { .. }));
    let templates = vec![TurnTemplate {
        kind: TemplateSelector::Expr,
        source: assemble_expression_module(
            &insert_preamble_imports(effects.preamble(), &imports),
            "__result",
            effects.row(),
            "{{TURN}}",
            ExpressionLift::Pure,
        ),
    }];
    let TurnResult::Expr { compiled, .. } = tidepool_testing::with_settlement(|settlement| {
        run_turn(
            TurnRequest {
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
            },
            settlement,
        )
    })
    .unwrap() else {
        panic!("recovered transitive call must compile as an expression");
    };
    assert_same_content(&recovery, &before);
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
        let ResidentOutcome::Completed { result, .. } =
            tidepool_testing::with_settlement(|settlement| {
                resident.run_with_sites(
                    "recovered_original_dependency",
                    compiled.code(),
                    settlement,
                )
            })
            .unwrap()
        else {
            panic!("real recovered original dependency must execute in each selected scope");
        };
        assert_eq!(result.to_json(), serde_json::json!(42));
        assert_same_content(&recovery, &before);
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
            .map(|template| {
                let kind = match template.kind {
                    TemplateSelector::Decl => "decl",
                    TemplateSelector::Bind => "bind",
                    TemplateSelector::BindDiscard => "binddiscard",
                    TemplateSelector::Expr => "expr",
                };
                (kind.into(), template.source.clone())
            })
            .collect(),
            injected_modules: view.injected_module_names(),
            reserved_declaration_modules: Vec::new(),
        });
        let include = view.include_paths(effects.include_paths());
        let plan = tidepool_testing::with_settlement(|settlement| {
            tidepool_toolchain::artifacts::parse_cell_plan(
                specification.clone(),
                &include,
                settlement,
            )
        })
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
        let (checked, program) = tidepool_testing::with_settlement(|settlement| {
            compile_cell_program_admitted(admission.clone(), settlement)
        })
        .unwrap();
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
        let outcome = tidepool_testing::with_settlement(|settlement| {
            resident.run_bind_with_sites(
                &binder.name,
                compiled.code(),
                binder,
                reservation.generation(),
                settlement,
            )
        })
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
        assert_same_content(&recovery, &before);
    }
    resident.retire_scope(private);
    assert_eq!(resident.value_handle_count(), handles_before);
    assert_eq!(
        resident.public_visibility_snapshot_in(public).unwrap(),
        public_before
    );
    assert_same_content(&recovery, &before);
    std::fs::write(
        &spec.result_file,
        serde_json::to_vec(&std::process::id()).unwrap(),
    )
    .unwrap();
}

fn assert_same_content(
    recovery: &RecoveryFixture,
    before: &super::recovery_store::RecoveryDomainState,
) {
    let current = recovery.state();
    assert_eq!(current.content_revision, before.content_revision);
    assert_eq!(current.snapshot_root, before.snapshot_root);
    assert!(current.allocation_ceiling >= before.allocation_ceiling);
}

fn persist_empty_public(recovery: &RecoveryFixture, id: u64) {
    let mut session = PersistentSession::new(Some(library(id, recovery, &[])), 1024);
    let public = session.mint_isolated_scope();
    assert_eq!(
        session
            .initialize_durable_public_scope(owner(1), public)
            .unwrap(),
        PublicManifestCommit::Durable
    );
}

#[test]
fn owned_domain_attachment_refuses_foreign_run_and_artifact_directory() {
    let durable = tempfile::tempdir().unwrap();
    let recovery = RecoveryFixture::create(durable.path());
    persist_empty_public(&recovery, 4410);
    let foreign = tempfile::tempdir().unwrap();
    let foreign_recovery = RecoveryFixture::create(foreign.path());
    let mut lib = SessionLib::open(
        SessionId(4411),
        durable.path().join("compiler"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    let artifact_root = lib.include_dir().canonicalize().unwrap();
    assert!(
        tidepool_testing::with_settlement(|settlement| lib.attach_owned_recovery_domain(
            recovery.store.clone(),
            super::recovery_store::RecoveryDomainId::Root,
            artifact_root.clone(),
            foreign_recovery.owner.clone(),
            settlement,
        ))
        .is_err()
    );
    assert!(lib.durable_graph.is_none());
    let foreign_artifacts = SessionLib::open(
        SessionId(4412),
        foreign.path().join("compiler"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    assert!(
        tidepool_testing::with_settlement(|settlement| lib.attach_owned_recovery_domain(
            recovery.store.clone(),
            super::recovery_store::RecoveryDomainId::Root,
            foreign_artifacts.include_dir().canonicalize().unwrap(),
            recovery.owner.clone(),
            settlement,
        ))
        .is_err()
    );
    assert!(lib.durable_graph.is_none());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
        .unwrap();
    let mut session = PersistentSession::new(Some(lib), 1024);
    assert!(session.recover_public_scope(&owner(2)).is_err());
    assert!(session.lib().durable_public_scopes.is_empty());
    session.recover_public_scope(&owner(1)).unwrap();
}

#[test]
fn successor_transfer_fences_old_admissions_after_commit_or_commit_error() {
    for uncertain in [false, true] {
        let durable = tempfile::tempdir().unwrap();
        let recovery = RecoveryFixture::create(durable.path());
        persist_empty_public(&recovery, 4420);
        let mut session = PersistentSession::new(Some(library(4421, &recovery, &[])), 1024);
        let target = session.mint_scope(ScopeId::ROOT).unwrap();
        let old = session.begin_private_execution(target).unwrap();
        let old_intent = session
            .freeze_execution_intent(&old, vec![], vec![])
            .unwrap();
        let before = session.public_visibility_snapshot_in(target).unwrap();
        if uncertain {
            recovery
                .store
                .inject_fault(super::recovery_store::StoreFault::CommitError);
        }
        let outcome = session
            .transfer_recovered_public_owner(
                &owner(1),
                owner(2),
                target,
                Arc::new(TestSuccessor {
                    run: recovery.owner.clone(),
                    session: SessionId(4421),
                    target,
                }),
            )
            .unwrap();
        if uncertain {
            assert!(matches!(
                outcome,
                PublicManifestCommit::RecoveryRequired { .. }
            ));
            assert_eq!(session.public_visibility_snapshot_in(target), Some(before));
            assert!(recovery.store.ensure_ready().is_err());
            assert!(session.begin_private_execution(target).is_err());
            let reopened = RecoveryFixture::reopen(recovery.owner.clone());
            let mut recovered = PersistentSession::new(Some(library(4422, &reopened, &[])), 1024);
            assert!(recovered.recover_public_scope(&owner(1)).is_err());
            let successor = recovered.recover_public_scope(&owner(2)).unwrap();
            assert_eq!(
                recovered
                    .public_visibility_snapshot_in(successor)
                    .unwrap()
                    .epoch,
                2
            );
            assert!(session.begin_private_execution(target).is_err());
        } else {
            assert_eq!(outcome, PublicManifestCommit::Durable);
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
        }
    }
}

#[test]
fn successor_transfer_retains_only_the_sealed_live_bootstrap_dependencies() {
    {
        let durable = tempfile::tempdir().unwrap();
        let recovery = RecoveryFixture::create(durable.path());
        persist_empty_public(&recovery, 4440);
        let run = recovery.owner.clone();
        let mut lib = SessionLib::open(
            SessionId(4441),
            recovery.store.path().parent().unwrap().join("compiler"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
            .unwrap();
        let mut session = PersistentSession::new(Some(lib), 1024 * 1024);
        let target = session.mint_isolated_scope();
        let (_, keys) = prepared::tests::install_source_publication_fixture(&mut session, target);
        assert_eq!(keys.len(), 2);
        let snapshot = session.public_visibility_snapshot_in(target).unwrap();
        assert_eq!(snapshot.declaration_tip, Generation(0));
        assert!(snapshot.bindings.is_empty());
        let before = domain_state(session.lib());
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
        assert_eq!(domain_state(session.lib()), before);
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
        assert_eq!(domain_state(session.lib()), before);
        let outcome = transfer(&mut session, target).unwrap();
        assert_eq!(outcome, PublicManifestCommit::Durable);
        let transferred = session.public_visibility_snapshot_in(target).unwrap();
        assert_eq!(transferred.source_instances, snapshot.source_instances);
        assert_eq!(
            transferred.machine_incarnation,
            snapshot.machine_incarnation
        );
        assert!(transferred.bindings.is_empty());
        assert_eq!(transferred.epoch, 2);
        let persisted = graph(session.lib());
        assert!(persisted
            .surface(&owner(2))
            .unwrap()
            .unwrap()
            .source_instances
            .is_empty());
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
        let recovery = RecoveryFixture::create(durable.path());
        persist_empty_public(&recovery, 4442);
        let run = recovery.owner.clone();
        let mut lib = SessionLib::open(
            SessionId(4443),
            recovery.store.path().parent().unwrap().join("compiler"),
            ModuleEnv::standalone_default(),
        )
        .unwrap();
        tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement))
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
        let before = recovery.state();
        assert!(matches!(
            session.transfer_recovered_public_owner(
                &owner(1), owner(2), target,
                Arc::new(TestSuccessor { run, session: SessionId(4443), target }),
            ),
            Err(SessionError::InvalidRecoveryInitialization { reason: actual, .. }) if actual == reason
        ));
        assert_eq!(recovery.state(), before);
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
    let recovery = RecoveryFixture::create(durable.path());
    persist_empty_public(&recovery, 4430);
    let previous = recovery
        .store
        .read_public_owner(super::recovery_store::RecoveryDomainId::Root, &owner(1))
        .unwrap()
        .unwrap();
    assert_eq!(recovery.state().allocation_ceiling, Generation(0));
    let reopened = RecoveryFixture::reopen(recovery.owner.clone());
    let mut session = PersistentSession::new(Some(library(4431, &reopened, &[])), 1024);
    assert!(graph(session.lib()).node_ids().next().is_none());
    let target = session.mint_isolated_scope();
    assert_eq!(
        session
            .transfer_recovered_public_owner(
                &owner(1),
                owner(2),
                target,
                Arc::new(TestSuccessor {
                    run: reopened.owner.clone(),
                    session: SessionId(4431),
                    target
                }),
            )
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert!(graph(session.lib()).node_ids().next().is_none());
    assert_eq!(reopened.state().allocation_ceiling, Generation(0));
    let successor = reopened
        .store
        .read_public_owner(super::recovery_store::RecoveryDomainId::Root, &owner(2))
        .unwrap()
        .unwrap();
    assert_eq!(successor.surface_root, previous.surface_root);
    assert_eq!(successor.epoch, previous.epoch + 1);
    assert!(reopened
        .store
        .read_public_owner(super::recovery_store::RecoveryDomainId::Root, &owner(1))
        .unwrap()
        .is_none());
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
    let recovery = RecoveryFixture::create(durable.path());
    let mut session = PersistentSession::new(Some(library(4407, &recovery, &[])), 1024);
    let parent = session.mint_isolated_scope();
    assert_eq!(
        session
            .initialize_durable_public_scope(owner(1), parent)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    let before_parent = session.public_visibility_snapshot_in(parent).unwrap();
    let parent_surface = recovery
        .store
        .read_public_owner(RecoveryDomainId::Root, &owner(1))
        .unwrap()
        .unwrap();
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
    let before = recovery.state();
    assert!(session
        .initialize_durable_public_scope(child_owner.clone(), parent)
        .is_err());
    assert_eq!(recovery.state(), before);
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
        recovery
            .store
            .read_public_owner(RecoveryDomainId::Root, &owner(1))
            .unwrap(),
        Some(parent_surface)
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
    let initialized = recovery.state();
    assert_eq!(
        session
            .initialize_durable_public_scope(child_owner.clone(), child)
            .unwrap(),
        PublicManifestCommit::Durable
    );
    assert_eq!(recovery.state(), initialized);
}

#[test]
fn durable_child_commit_error_requires_reopen_without_reinitializing_or_reexecuting() {
    let durable = tempfile::tempdir().unwrap();
    let recovery = RecoveryFixture::create(durable.path());
    let mut session = PersistentSession::new(Some(library(4408, &recovery, &[])), 1024);
    let parent = session.mint_isolated_scope();
    session
        .initialize_durable_public_scope(owner(1), parent)
        .unwrap();
    let parent_before = session.public_visibility_snapshot_in(parent).unwrap();
    let child = session.mint_detached_scope(parent).unwrap();
    let child_before = session.public_visibility_snapshot_in(child).unwrap();
    let child_owner =
        RecoveryPublicOwner::new(&ActorPath::parse("root/exact-recovery/child").unwrap(), 3)
            .unwrap();
    recovery
        .store
        .inject_fault(super::recovery_store::StoreFault::CommitError);
    assert!(matches!(
        session
            .initialize_durable_public_scope(child_owner.clone(), child)
            .unwrap(),
        PublicManifestCommit::RecoveryRequired { .. }
    ));
    assert_eq!(
        session.public_visibility_snapshot_in(parent),
        Some(parent_before)
    );
    assert_eq!(
        session.public_visibility_snapshot_in(child),
        Some(child_before)
    );
    assert!(session
        .begin_durable_private_execution(&child_owner, child)
        .is_err());
    assert!(session
        .begin_durable_private_execution(&owner(1), parent)
        .is_err());
    let reopened = RecoveryFixture::reopen(recovery.owner.clone());
    let durable_before = reopened.state();
    let mut recovered = PersistentSession::new(Some(library(4409, &reopened, &[])), 1024);
    let parent = recovered.recover_public_scope(&owner(1)).unwrap();
    let child = recovered.recover_public_scope(&child_owner).unwrap();
    assert_eq!(
        recovered
            .begin_durable_private_execution(&owner(1), parent)
            .unwrap()
            .admitted_public()
            .epoch,
        1
    );
    assert_eq!(
        recovered
            .begin_durable_private_execution(&child_owner, child)
            .unwrap()
            .admitted_public()
            .epoch,
        1
    );
    assert_eq!(reopened.state(), durable_before);
    assert!(session.begin_private_execution(ScopeId::ROOT).is_err());
}
