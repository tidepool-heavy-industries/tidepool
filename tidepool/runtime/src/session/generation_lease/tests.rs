use super::*;
use crate::session::exact_recovery_acceptance_tests::{run_owner, TestRunOwner};
use crate::session::recovery_store::{
    RecoveryDomainId, RecoveryDomainState, RunRecoveryStore, StoreFault,
};
use crate::session::{ModuleEnv, RecoveryRunAuthority, ScopeId, SessionLib};
use std::sync::Arc;

struct TestRun {
    directory: tempfile::TempDir,
    authority: Arc<TestRunOwner>,
    store: Arc<RunRecoveryStore>,
}
impl TestRun {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let authority = run_owner(directory.path());
        let store =
            Arc::new(RunRecoveryStore::create(&directory.path().join("recovery.sqlite")).unwrap());
        Self {
            directory,
            authority,
            store,
        }
    }
    fn attach(&self, domain: RecoveryDomainId) -> Result<SessionLib, crate::session::SessionError> {
        let session = match domain {
            RecoveryDomainId::Root => crate::session::registry::fresh_session_id(),
            RecoveryDomainId::Session(session) => session,
        };
        let artifact_root = match domain {
            RecoveryDomainId::Root => self.directory.path().join("root-artifacts"),
            RecoveryDomainId::Session(session) => self
                .directory
                .path()
                .join("children")
                .join(session.0.to_string()),
        };
        let mut lib = SessionLib::open(session, &artifact_root, ModuleEnv::standalone_default())?;
        tidepool_testing::with_settlement(|settlement| {
            lib.attach_owned_recovery_domain(
                self.store.clone(),
                domain,
                artifact_root,
                self.authority.clone(),
                settlement,
            )
        })?;
        Ok(lib)
    }
    fn root(&self) -> SessionLib {
        self.attach(RecoveryDomainId::Root).unwrap()
    }
    fn child(&self) -> SessionLib {
        self.attach(RecoveryDomainId::Session(
            crate::session::registry::fresh_session_id(),
        ))
        .unwrap()
    }
    fn reopen(&mut self) {
        assert!(self.authority.owns_run(self.directory.path()).unwrap());
        self.store = Arc::new(RunRecoveryStore::open(self.store.path()).unwrap());
    }
}
fn store_state(lib: &SessionLib) -> RecoveryDomainState {
    let state = lib.durable_graph.as_ref().unwrap();
    state.store.read_domain(state.domain_id).unwrap().unwrap()
}

#[test]
fn refill_covers_shortage_large_request_and_final_partial_range() {
    assert_eq!(refill_ceiling(Generation(0), Generation(1)), Generation(64));
    assert_eq!(
        refill_ceiling(Generation(64), Generation(65)),
        Generation(128)
    );
    assert_eq!(
        refill_ceiling(Generation(64), Generation(200)),
        Generation(200)
    );
    assert_eq!(
        refill_ceiling(Generation(u64::MAX - 7), Generation(u64::MAX - 6)),
        Generation(u64::MAX)
    );
}

#[test]
fn confirmed_tail_at_max_is_consumable_then_typed_exhaustion() {
    let lease = GenerationRangeLease::confirmed(Generation(u64::MAX));
    let end = requested_end(Generation(u64::MAX - 1), 1).unwrap();
    assert!(lease.covers(end));
    assert_eq!(
        requested_end(end, 1),
        Err(GenerationExhausted {
            consumed: end,
            requested: 1
        })
    );
    assert_eq!(requested_end(end, 0).unwrap(), end);
}

#[test]
fn session_consumes_only_requested_slots_without_durable_writes_within_lease() {
    let run = TestRun::new();
    let mut lib = run.root();
    assert_eq!(
        lib.reserve_declaration_generations_durable(2).unwrap(),
        vec![Generation(1), Generation(2)]
    );
    let committed = store_state(&lib);
    let work = run.store.work();
    assert_eq!(committed.allocation_ceiling, Generation(64));
    assert_eq!(lib.generation(), Generation(2));
    assert_eq!(lib.next_module().unwrap().gen(), Generation(3));
    assert!(!lib.log.is_reserved(Generation(3)));
    assert!(!lib.log.is_reserved(Generation(64)));
    assert_eq!(
        lib.reserve_declaration_generations_durable(62)
            .unwrap()
            .len(),
        62
    );
    assert_eq!(store_state(&lib), committed);
    assert_eq!(run.store.work(), work);
    assert_eq!(lib.generation(), Generation(64));
    assert_eq!(
        lib.reserve_declaration_generation_durable().unwrap(),
        Generation(65)
    );
    assert_eq!(store_state(&lib).allocation_ceiling, Generation(128));
    assert_eq!(run.store.work().transactions, work.transactions + 1);
    assert_eq!(lib.scope_tip(ScopeId::ROOT), Generation(0));
    assert!(!lib.log.is_reserved(Generation(66)));
}

#[test]
fn session_restart_burns_unused_tail_and_child_inherits_ceiling_without_lease() {
    let mut run = TestRun::new();
    let mut parent = run.root();
    assert_eq!(
        parent.reserve_declaration_generation_durable().unwrap(),
        Generation(1)
    );
    let floor = parent.durable_allocation_ceiling().unwrap();
    assert_eq!(floor, store_state(&parent).allocation_ceiling);
    let mut child = run.child();
    child
        .initialize_captured_declaration_high_water(floor)
        .unwrap();
    assert!(child.generation_lease.is_none());
    assert_eq!(child.generation(), floor);
    assert!(!child.log.is_reserved(floor));
    assert_eq!(
        child.reserve_declaration_generation_durable().unwrap(),
        Generation(65)
    );
    assert_eq!(
        parent.reserve_declaration_generation_durable().unwrap(),
        Generation(2)
    );
    drop(parent);
    drop(child);
    run.reopen();
    let mut restarted = run.root();
    assert!(restarted.generation_lease.is_none());
    assert_eq!(restarted.generation(), floor);
    assert_eq!(
        restarted.reserve_declaration_generation_durable().unwrap(),
        Generation(65)
    );
}

#[test]
fn session_full_u64_tail_large_request_and_import_after_exposure_refusal() {
    let run = TestRun::new();
    let mut lib = run.root();
    lib.initialize_captured_declaration_high_water(Generation(u64::MAX - 80))
        .unwrap();
    let generations = lib.reserve_declaration_generations_durable(70).unwrap();
    assert_eq!(generations.first(), Some(&Generation(u64::MAX - 79)));
    assert_eq!(generations.last(), Some(&Generation(u64::MAX - 10)));
    assert_eq!(
        store_state(&lib).allocation_ceiling,
        Generation(u64::MAX - 10)
    );
    assert!(lib
        .initialize_captured_declaration_high_water(Generation(u64::MAX))
        .is_err());
    assert_eq!(
        lib.reserve_declaration_generation_durable().unwrap(),
        Generation(u64::MAX - 9)
    );
    let tail = store_state(&lib);
    let work = run.store.work();
    assert_eq!(tail.allocation_ceiling, Generation(u64::MAX));
    assert_eq!(
        lib.reserve_declaration_generations_durable(9)
            .unwrap()
            .last(),
        Some(&Generation(u64::MAX))
    );
    assert_eq!(store_state(&lib), tail);
    assert_eq!(run.store.work(), work);
    assert!(matches!(
        lib.reserve_declaration_generation_durable(),
        Err(crate::session::SessionError::GenerationExhausted(_))
    ));
    assert!(matches!(
        lib.next_module(),
        Err(crate::session::SessionError::GenerationExhausted(_))
    ));
    assert_eq!(store_state(&lib), tail);
    assert_eq!(run.store.work(), work);
}

#[test]
fn session_reservation_faults_expose_no_ids_and_require_authenticated_reopen() {
    for fault in [
        StoreFault::BeforeCommit,
        StoreFault::CommitError,
        StoreFault::RollbackError,
    ] {
        let mut run = TestRun::new();
        let mut lib = run.root();
        let baseline = store_state(&lib);
        run.store.inject_fault(fault);
        assert!(lib.reserve_declaration_generations_durable(3).is_err());
        assert!(!lib.local_generation_issued);
        assert!(lib.generation_lease.is_none());
        assert_eq!(lib.generation(), Generation(0));
        for id in [1, 3, 64] {
            assert!(!lib.log.is_reserved(Generation(id)));
        }
        if fault == StoreFault::BeforeCommit {
            assert_eq!(store_state(&lib), baseline);
            assert!(!run.store.is_recovery_required());
            assert_eq!(
                lib.reserve_declaration_generation_durable().unwrap(),
                Generation(1)
            );
        } else {
            assert!(run.store.is_recovery_required());
            assert!(lib.durable_allocation_ceiling().is_err());
            assert!(lib.next_module().is_err());
            assert!(lib.reserve_declaration_generation_durable().is_err());
            let old_store = run.store.clone();
            drop(lib);
            run.reopen();
            assert!(old_store.ensure_ready().is_err());
            let mut restarted = run.root();
            let expected_ceiling = if fault == StoreFault::CommitError {
                Generation(64)
            } else {
                Generation(0)
            };
            assert_eq!(
                restarted.durable_allocation_ceiling().unwrap(),
                expected_ceiling
            );
            assert_eq!(restarted.generation(), expected_ceiling);
            assert!(restarted.generation_lease.is_none());
            assert_eq!(
                restarted.reserve_declaration_generation_durable().unwrap(),
                Generation(expected_ceiling.0 + 1)
            );
        }
    }
}

#[test]
fn uncertain_refill_fences_previously_confirmed_tail() {
    let mut run = TestRun::new();
    let mut lib = run.root();
    assert_eq!(
        lib.reserve_declaration_generation_durable().unwrap(),
        Generation(1)
    );
    run.store.inject_fault(StoreFault::CommitError);
    assert!(lib.reserve_declaration_generations_durable(64).is_err());
    assert!(lib.generation_lease.is_none());
    assert_eq!(lib.generation(), Generation(1));
    assert!(!lib.log.is_reserved(Generation(2)));
    assert!(lib.reserve_declaration_generation_durable().is_err());
    drop(lib);
    run.reopen();
    let mut lib = run.root();
    assert_eq!(lib.generation(), Generation(128));
    assert_eq!(
        lib.reserve_declaration_generation_durable().unwrap(),
        Generation(129)
    );
}

#[test]
fn fenced_run_refuses_value_only_cell_and_private_scope_activation() {
    let run = TestRun::new();
    let mut sibling = run.child();
    let mut session = crate::session::PersistentSession::new(Some(run.root()), 1024 * 1024);
    let public = session.mint_scope(ScopeId::ROOT).unwrap();
    let private = Arc::new(session.begin_private_execution(public).unwrap());
    let accepted = session
        .admit_cell_for_execution(
            private.clone(),
            0,
            Arc::new(()),
            [7; 32],
            [8; 32],
            Vec::new(),
        )
        .unwrap();
    assert!(accepted.visibility().machine_incarnation.is_some());
    assert_eq!(session.lib().generation(), Generation(0));
    let value_generation = session.val_gen();
    let scopes = session.scope_tree().len();
    run.store.inject_fault(StoreFault::CommitError);
    assert!(sibling.reserve_declaration_generation_durable().is_err());
    assert!(session
        .admit_cell_for_execution(private, 0, Arc::new(()), [7; 32], [8; 32], Vec::new())
        .is_err());
    assert_eq!(session.val_gen(), value_generation);
    assert_eq!(session.scope_tree().len(), scopes);
    assert!(session.begin_private_execution(public).is_err());
    assert_eq!(session.scope_tree().len(), scopes);
    assert_eq!(session.lib().generation(), Generation(0));
}

#[test]
fn fenced_run_refuses_prepared_item_admission_and_native_start() {
    use crate::session::turn::{compile_cell_program_admitted, TemplateSelector};
    use crate::session::workbench::{resident_cell_check_template, resident_workbench_templates};
    use tidepool_toolchain::checked_cell::CheckedCellSpecification;

    tidepool_testing::eval_harness::require_extract();
    let effects = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    for (source, native_start) in [
        ("leaseChecked :: Int\nleaseChecked = 42", false),
        ("leaseValue <- pure (42 :: Int)", true),
    ] {
        let run = TestRun::new();
        let mut sibling = run.child();
        let lib = run
            .root()
            .with_validation_include(effects.include_paths().to_vec());
        let mut session = crate::session::PersistentSession::new(Some(lib), 1024 * 1024);
        let public = session.mint_scope(ScopeId::ROOT).unwrap();
        let execution = Arc::new(session.begin_private_execution(public).unwrap());
        let view = session.compile_view_for_execution(&execution).unwrap();
        let imports = view.turn_imports(&crate::session::SourceImports::new());
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
            panic!("checked lease fixture must contain one source item");
        };
        let native = item.native().cloned();
        let item = item.checked_item().clone();
        let prefix = session
            .begin_cell_program(admission, program)
            .unwrap()
            .unwrap();
        let generation = if native_start {
            Generation(0)
        } else {
            Generation(1)
        };
        assert_eq!(session.lib().generation(), generation);
        if native_start {
            session
                .admit_checked_item(prefix.clone(), item.clone())
                .unwrap();
        } else {
            assert!(session.lib().log.is_reserved(generation));
        }
        let scopes = session.scope_tree().len();
        run.store.inject_fault(StoreFault::CommitError);
        assert!(sibling.reserve_declaration_generation_durable().is_err());
        if native_start {
            assert!(matches!(
                prefix.start(
                    &session,
                    prefix.admission().visibility().scope,
                    native.unwrap()
                ),
                Err(crate::session::SessionError::RecoveryManifest { .. })
            ));
        } else {
            assert!(matches!(
                session.admit_checked_item(prefix.clone(), item),
                Err(crate::session::SessionError::RecoveryManifest { .. })
            ));
        }
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);
        assert_eq!(session.scope_tree().len(), scopes);
        assert_eq!(session.lib().generation(), generation);
    }
}

#[test]
fn failed_compiler_validation_burns_requested_identity_and_restart_burns_tail() {
    tidepool_testing::eval_harness::require_extract();
    let mut run = TestRun::new();
    let mut lib = run
        .root()
        .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
    let receipt = tidepool_testing::with_settlement(|settlement| {
        lib.declaration_receipt(&["answer :: Int\nanswer = missingHelper"], settlement)
    })
    .unwrap()
    .unwrap();
    let candidate = lib
        .render_admitted_candidate_in(
            ScopeId::ROOT,
            &crate::session::SourceImports::new(),
            &receipt,
            &[],
            &[],
        )
        .unwrap();
    assert!(tidepool_testing::with_settlement(|settlement| {
        crate::session::validate_declaration_candidate(candidate, lib.include_dir(), settlement)
    })
    .is_err());
    assert!(lib.log.is_reserved(Generation(1)));
    assert!(!lib.log.is_reserved(Generation(2)));
    assert_eq!(lib.scope_tip(ScopeId::ROOT), Generation(0));
    assert_eq!(store_state(&lib).allocation_ceiling, Generation(64));
    drop(lib);
    run.reopen();
    let mut restored = run.root();
    assert_eq!(restored.generation(), Generation(64));
    assert_eq!(
        restored.reserve_join_generation_durable().unwrap(),
        Generation(65)
    );
}

#[test]
fn resident_child_capture_reads_ceiling_and_refuses_uncertain_floor() {
    #[derive(Clone)]
    struct QuietOutput;
    impl crate::session::OutputSink for QuietOutput {
        fn drain(&self) -> Vec<String> {
            Vec::new()
        }
        fn snapshot(&self) -> Vec<String> {
            Vec::new()
        }
    }
    let mut run = TestRun::new();
    let mut lib = run.root();
    run.store.inject_fault(StoreFault::CommitError);
    assert!(lib.reserve_declaration_generation_durable().is_err());
    let session = crate::session::ResidentSession::unbootstrapped(
        frunk::HNil,
        QuietOutput,
        crate::DEFAULT_NURSERY_SIZE,
        Some(lib),
    );
    assert!(session.declaration_generation_high_water().is_err());
    drop(session);
    run.reopen();
    let session = crate::session::ResidentSession::unbootstrapped(
        frunk::HNil,
        QuietOutput,
        crate::DEFAULT_NURSERY_SIZE,
        Some(run.root()),
    );
    assert_eq!(
        session.declaration_generation_high_water().unwrap(),
        Some(Generation(64))
    );
}

#[test]
fn sibling_domain_uncertainty_revokes_every_held_run_lease() {
    let mut run = TestRun::new();
    let mut root = run.root();
    let mut sibling = run.child();
    let sibling_domain = sibling.durable_graph.as_ref().unwrap().domain_id;
    assert_eq!(
        root.reserve_declaration_generation_durable().unwrap(),
        Generation(1)
    );
    assert_eq!(
        sibling.reserve_declaration_generation_durable().unwrap(),
        Generation(1)
    );
    run.store.inject_fault(StoreFault::CommitError);
    assert!(sibling.reserve_declaration_generations_durable(64).is_err());
    assert!(root.generation_lease.is_some());
    assert!(root.next_module().is_err());
    assert!(root.durable_allocation_ceiling().is_err());
    assert!(root.reserve_declaration_generation_durable().is_err());
    assert!(root.generation_lease.is_none());
    assert_eq!(root.generation(), Generation(1));
    assert!(!root.log.is_reserved(Generation(2)));
    let fenced_store = run.store.clone();
    drop(root);
    drop(sibling);
    run.reopen();
    assert!(fenced_store.ensure_ready().is_err());
    assert_eq!(
        run.store
            .read_domain(sibling_domain)
            .unwrap()
            .unwrap()
            .allocation_ceiling,
        Generation(128)
    );
    let mut root = run.root();
    assert_eq!(root.generation(), Generation(64));
    assert_eq!(
        root.reserve_declaration_generation_durable().unwrap(),
        Generation(65)
    );
}

#[test]
fn domains_refuse_collisions_and_preserve_independent_ceilings() {
    let run = TestRun::new();
    let mut root = run.root();
    let mut first = run.child();
    let mut second = run.child();
    let first_domain = first.durable_graph.as_ref().unwrap().domain_id;
    assert!(run.attach(first_domain).is_err());
    first
        .initialize_captured_declaration_high_water(Generation(1 << 40))
        .unwrap();
    let first_state = store_state(&first);
    let work = run.store.work();
    first
        .initialize_captured_declaration_high_water(Generation(4))
        .unwrap();
    assert_eq!(store_state(&first), first_state);
    assert_eq!(run.store.work(), work);
    assert_eq!(
        root.reserve_declaration_generation_durable().unwrap(),
        Generation(1)
    );
    assert_eq!(
        second.reserve_declaration_generation_durable().unwrap(),
        Generation(1)
    );
    assert_eq!(
        first.reserve_declaration_generation_durable().unwrap(),
        Generation((1 << 40) + 1)
    );
    assert_eq!(store_state(&root).allocation_ceiling, Generation(64));
    assert_eq!(store_state(&second).allocation_ceiling, Generation(64));
    assert_eq!(
        store_state(&first).allocation_ceiling,
        Generation((1 << 40) + 64)
    );
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config { cases: 128, ..Default::default() })]
    #[test]
    fn full_u64_arithmetic_matches_independent_wide_integer_model(
        consumed in proptest::prelude::any::<u64>(),
        amount in proptest::prelude::any::<u64>(),
        other in proptest::prelude::any::<u64>(),
    ) {
        let wide_end = u128::from(consumed) + u128::from(amount);
        let actual_end = requested_end(Generation(consumed), amount);
        if wide_end > u128::from(u64::MAX) {
            proptest::prop_assert_eq!(actual_end, Err(GenerationExhausted { consumed: Generation(consumed), requested: amount }));
        } else {
            proptest::prop_assert_eq!(actual_end.unwrap().0, wide_end as u64);
        }
        let ceiling = consumed.min(other);
        let end = consumed.max(other);
        if ceiling < end {
            let expected = (u128::from(ceiling) + 64).max(u128::from(end)).min(u128::from(u64::MAX));
            let actual = refill_ceiling(Generation(ceiling), Generation(end));
            proptest::prop_assert_eq!(u128::from(actual.0), expected);
            proptest::prop_assert!(actual.0 >= end);
        }
    }
}

#[derive(Clone, Debug)]
enum HistoryOperation {
    Reserve(u16),
    Cancel(u16),
    Restart,
    Fork,
    Import(u16),
    ReservationFault(u8),
}

fn history_operation() -> impl proptest::strategy::Strategy<Value = HistoryOperation> {
    use proptest::strategy::{Just, Strategy};
    proptest::prop_oneof![
        3 => (0u16..160).prop_map(HistoryOperation::Reserve),
        2 => (0u16..160).prop_map(HistoryOperation::Cancel),
        1 => Just(HistoryOperation::Restart),
        1 => Just(HistoryOperation::Fork),
        1 => (0u16..160).prop_map(HistoryOperation::Import),
        1 => (0u8..3).prop_map(HistoryOperation::ReservationFault),
    ]
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config { cases: 24, ..Default::default() })]
    #[test]
    fn session_histories_never_reissue_and_bound_durable_writes(
        start in proptest::prop_oneof![proptest::strategy::Just(0u64), (u64::MAX - 300)..=u64::MAX],
        history in proptest::collection::vec(history_operation(), 1..48),
    ) {
        let mut run = TestRun::new();
        let mut lib = run.root();
        lib.initialize_captured_declaration_high_water(Generation(start)).unwrap();
        let mut consumed = u128::from(start);
        let mut ceiling = u128::from(start);
        let mut issued = std::collections::BTreeSet::new();
        let mut lease = false;
        let mut exposed = false;
        for operation in history {
            match operation {
                HistoryOperation::Reserve(amount) | HistoryOperation::Cancel(amount) => {
                    let count = usize::from(amount);
                    let before = store_state(&lib);
                    let work = run.store.work();
                    let end = consumed + count as u128;
                    let result = lib.reserve_declaration_generations_durable(count);
                    if end > u128::from(u64::MAX) {
                        proptest::prop_assert!(matches!(result, Err(crate::session::SessionError::GenerationExhausted(_))));
                        proptest::prop_assert_eq!(store_state(&lib), before);
                        proptest::prop_assert_eq!(run.store.work(), work);
                    } else {
                        let ids = result.unwrap();
                        proptest::prop_assert_eq!(ids.len(), count);
                        for (index, id) in ids.iter().enumerate() {
                            proptest::prop_assert_eq!(u128::from(id.0), consumed + index as u128 + 1);
                            proptest::prop_assert!(issued.insert(*id));
                            proptest::prop_assert!(lib.log.is_reserved(*id));
                        }
                        if count != 0 && (!lease || end > ceiling) {
                            // Independent integer model: transaction reserves
                            // the greater of demand and one 64-ID block.
                            ceiling = (ceiling + 64).max(end).min(u128::from(u64::MAX));
                            lease = true;
                            proptest::prop_assert_ne!(store_state(&lib).revision, before.revision);
                            proptest::prop_assert_eq!(run.store.work().transactions, work.transactions + 1);
                            proptest::prop_assert_eq!(run.store.work().block_writes, work.block_writes);
                            proptest::prop_assert_eq!(run.store.work().block_bytes, work.block_bytes);
                        } else {
                            proptest::prop_assert_eq!(store_state(&lib), before);
                            proptest::prop_assert_eq!(run.store.work(), work);
                        }
                        consumed = end;
                        exposed |= count != 0;
                        proptest::prop_assert_eq!(u128::from(store_state(&lib).allocation_ceiling.0), ceiling);
                        if consumed < ceiling {
                            proptest::prop_assert!(!lib.log.is_reserved(Generation(consumed as u64 + 1)));
                        }
                        // Dropping a candidate does not return its identity.
                    }
                }
                HistoryOperation::Restart => {
                    drop(lib);
                    run.reopen();
                    lib = run.root();
                    consumed = ceiling;
                    lease = false;
                    exposed = false;
                    proptest::prop_assert!(lib.generation_lease.is_none());
                }
                HistoryOperation::Fork => {
                    let mut child = run.child();
                    child.initialize_captured_declaration_high_water(Generation(ceiling as u64)).unwrap();
                    proptest::prop_assert_eq!(u128::from(child.generation().0), ceiling);
                    proptest::prop_assert!(child.generation_lease.is_none());
                    let child_result = child.reserve_declaration_generation_durable();
                    if ceiling == u128::from(u64::MAX) {
                        proptest::prop_assert!(matches!(child_result, Err(crate::session::SessionError::GenerationExhausted(_))));
                    } else {
                        proptest::prop_assert_eq!(u128::from(child_result.unwrap().0), ceiling + 1);
                    }
                }
                HistoryOperation::Import(offset) => {
                    let floor = (ceiling + u128::from(offset)).min(u128::from(u64::MAX));
                    let before = store_state(&lib);
                    let result = lib.initialize_captured_declaration_high_water(Generation(floor as u64));
                    if exposed {
                        proptest::prop_assert!(result.is_err());
                        proptest::prop_assert_eq!(store_state(&lib), before);
                    } else {
                        result.unwrap();
                        consumed = floor;
                        ceiling = floor;
                        lease = false;
                        proptest::prop_assert!(lib.generation_lease.is_none());
                    }
                }
                HistoryOperation::ReservationFault(kind) => {
                    if ceiling < u128::from(u64::MAX) {
                        let fault = match kind {
                            0 => StoreFault::BeforeCommit,
                            1 => StoreFault::CommitError,
                            _ => StoreFault::RollbackError,
                        };
                        let before = store_state(&lib);
                        let count = usize::try_from(ceiling - consumed + 1).unwrap();
                        run.store.inject_fault(fault);
                        proptest::prop_assert!(lib.reserve_declaration_generations_durable(count).is_err());
                        proptest::prop_assert!(!lib.log.is_reserved(Generation(consumed as u64 + 1)));
                        if kind == 0 {
                            proptest::prop_assert_eq!(store_state(&lib), before);
                        } else {
                            if kind == 1 {
                                ceiling = (ceiling + 64).min(u128::from(u64::MAX));
                            }
                            proptest::prop_assert!(run.store.is_recovery_required());
                            proptest::prop_assert!(lib.reserve_declaration_generation_durable().is_err());
                            proptest::prop_assert!(lib.durable_allocation_ceiling().is_err());
                            let fenced_store = run.store.clone();
                            drop(lib);
                            run.reopen();
                            lib = run.root();
                            proptest::prop_assert!(fenced_store.ensure_ready().is_err());
                            proptest::prop_assert_eq!(u128::from(store_state(&lib).allocation_ceiling.0), ceiling);
                            consumed = ceiling;
                            lease = false;
                            exposed = false;
                            proptest::prop_assert!(lib.generation_lease.is_none());
                        }
                    }
                }
            }
            proptest::prop_assert_eq!(u128::from(lib.generation().0), consumed);
        }
    }
}
