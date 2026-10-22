//! Runtime integration histories for machine-local native choices.

use super::*;

#[test]
fn sibling_native_slot_install_refuses_recorded_vacancy_then_reuses_live_group() {
    use tidepool_codegen::prepared_program::NativeBindingKey;
    tidepool_testing::eval_harness::require_extract();
    let targets = tidepool_testing::with_settlement(|settlement| {
        crate::compile_runtime_targets(
            &tidepool_toolchain::artifacts::CompileInvocation {
                source: include_str!("../../tests/fixtures/WorldPackageRuntime.hs"),
                targets: &["first"],
                include: &[tidepool_testing::eval_harness::prelude_path()],
                fallback_module_name: "WorldPackageRuntime",
            },
            |_, _, _| {},
            settlement,
        )
    })
    .expect("compile a real paired target with executable HOME and PACKAGE imports");
    let compiled =
        super::super::turn::CompiledTurn::from_runtime_target(&targets, "first").unwrap();
    let code = compiled.code();
    let mut session = PersistentSession::new(None, 1024 * 1024);
    let plan = Arc::new(
        super::super::prepared::NativeInstantiationPlan::prepare_code(
            &code,
            &session.certified_image_registry(),
        )
        .unwrap(),
    );
    let plan = super::super::prepared::ClosedInstantiationPlan::restrict(plan).unwrap();
    let origin = super::super::resident::CapturedSourceOrigin::from_runtime_targets(&targets);
    let admitted = session
        .bindings
        .scope_snapshot(&session.scopes, ScopeId::ROOT)
        .unwrap();
    let a = session
        .mint_world_execution_scope(ScopeId::ROOT, &admitted)
        .unwrap();
    let b = session
        .mint_world_execution_scope(ScopeId::ROOT, &admitted)
        .unwrap();
    let isolated = session.mint_detached_scope(ScopeId::ROOT).unwrap();
    let a_lease = session.prepared_lexical_scope_lease(a).unwrap();
    let b_lease = session.prepared_lexical_scope_lease(b).unwrap();
    let isolated_lease = session.prepared_lexical_scope_lease(isolated).unwrap();
    let settled = session
        .with_source_compiler_transaction(
            targets.source.clone(),
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            crate::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |session, _| {
                session.admit_source_lease(a_lease.clone()).unwrap();
                session.admit_source_lease(b_lease.clone()).unwrap();
                session.admit_source_lease(isolated_lease.clone()).unwrap();
                let stale = session.resolve_closed_turn_in(a, &plan, &origin).unwrap();
                let before_a = session.bindings.scope_snapshot(&session.scopes, a).unwrap();
                let (b_program, _, _) = session
                    .install_closed_turn_in(b, compiled.table(), &plan, &origin)
                    .unwrap()
                    .into_parts();
                let native_groups = |session: &PersistentSession, scope| {
                    let mut groups = BTreeMap::new();
                    for lease in session
                        .bindings
                        .selected_source_instances_in(&session.scopes, scope)
                    {
                        let old = groups.insert(lease.binding_key(), lease.instance());
                        assert!(
                            old.is_none_or(|old| old == lease.instance()),
                            "one selected native binding has one physical group"
                        );
                    }
                    groups
                };
                let b_groups = native_groups(session, b);
                assert!(
                    b_groups
                        .keys()
                        .any(|key| matches!(key, NativeBindingKey::Home(_))),
                    "the real fixture must demand HOME custody"
                );
                assert!(
                    b_groups
                        .keys()
                        .any(|key| matches!(key, NativeBindingKey::Package(_))),
                    "the real fixture must demand PACKAGE custody"
                );
                let residency = session.residency();
                let roots = session.persistent_roots_count();
                let table = session.session_table().clone();
                assert!(matches!(
                    session.install_native_turn_in(a, compiled.table(), stale),
                    Err(PreparedRuntimeError::SourceScopeAdmission)
                ));
                assert_eq!(
                    session.residency(),
                    residency,
                    "stale vacancy refuses before batch staging"
                );
                assert_eq!(
                    session.bindings.scope_snapshot(&session.scopes, a).unwrap(),
                    before_a
                );
                assert_eq!(session.persistent_roots_count(), roots);
                assert_eq!(session.session_table(), &table);
                let (a_program, _, _) = session
                    .install_closed_turn_in(a, compiled.table(), &plan, &origin)
                    .unwrap()
                    .into_parts();
                let a_groups = native_groups(session, a);
                assert!(
                    a_groups
                        .keys()
                        .any(|key| matches!(key, NativeBindingKey::Home(_))),
                    "fresh sibling resolution must actually request a HOME group"
                );
                // Anchored groups already retain their transitive dependencies;
                // A acquires only its actual requested exports from those groups.
                for (binding, instance) in a_groups {
                    assert_eq!(
                        b_groups.get(&binding),
                        Some(&instance),
                        "fresh resolution must select the exact live sibling group"
                    );
                }
                let captured = session.mint_detached_scope(b).unwrap();
                let captured_lease = session.prepared_lexical_scope_lease(captured).unwrap();
                session.admit_source_lease(captured_lease.clone()).unwrap();
                let current = |session: &PersistentSession, scope| {
                    session
                        .bindings
                        .native_domain_selection_in(&session.scopes, scope)
                        .unwrap()
                        .current()
                };
                assert_ne!(current(session, isolated), current(session, b));
                assert_ne!(current(session, captured), current(session, b));
                let (isolated_program, _, _) = session
                    .install_closed_turn_in(isolated, compiled.table(), &plan, &origin)
                    .unwrap()
                    .into_parts();
                let isolated_groups = native_groups(session, isolated);
                assert_eq!(
                    isolated_groups.keys().collect::<Vec<_>>(),
                    b_groups.keys().collect::<Vec<_>>()
                );
                for (binding, instance) in isolated_groups {
                    assert_ne!(
                        b_groups[&binding], instance,
                        "an empty independent fork instantiates its own native group"
                    );
                }
                let (captured_program, _, _) = session
                    .install_closed_turn_in(captured, compiled.table(), &plan, &origin)
                    .unwrap()
                    .into_parts();
                assert_eq!(
                    native_groups(session, captured),
                    b_groups,
                    "fork relabeling preserves actual selected physical groups"
                );
                for program in [a_program, b_program, isolated_program, captured_program] {
                    assert!(session.prepared_mut().unwrap().unpin(program));
                }
                (captured, captured_lease, b_groups)
            },
        )
        .unwrap()
        .into_outcome();
    assert!(matches!(
        settled.close,
        crate::CompilerTransactionClose::Clean | crate::CompilerTransactionClose::NotStarted
    ));
    let (captured, _captured_lease, groups) = settled.action;
    session.retire_scope(a);
    session.retire_scope(b);
    session.retire_scope(isolated);
    session
        .prepared_mut()
        .unwrap()
        .quiesce_and_collect_now()
        .unwrap();
    assert!(
        session.residency().unwrap().programs > 0,
        "the live captured scope retains its original groups"
    );
    let retained = session
        .bindings
        .selected_source_instances_in(&session.scopes, captured)
        .into_iter()
        .map(|lease| (lease.binding_key(), lease.instance()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        retained, groups,
        "every captured group survives its original scope retirement"
    );
    session.retire_scope(captured);
    session
        .prepared_mut()
        .unwrap()
        .quiesce_and_collect_now()
        .unwrap();
    assert_eq!(session.persistent_roots_count(), 0);
    assert_eq!(session.residency().unwrap().programs, 0);
}

#[test]
fn refused_install_retains_primary_and_uncertain_abort_separately() {
    use super::super::prepared::{PreparedFailureKind, PreparedFailureStage};
    use std::error::Error;
    use tidepool_codegen::prepared_program::ExecutionError;

    let refused = PreparedRuntimeError::SourceScopeAdmission.after_install_abort(Ok(()));
    assert!(matches!(
        refused,
        PreparedRuntimeError::SourceScopeAdmission
    ));
    assert_eq!(refused.stage(), PreparedFailureStage::Install);
    assert_eq!(refused.kind(), PreparedFailureKind::Rejected);

    let uncertain = PreparedRuntimeError::SourceScopeAdmission
        .after_install_abort(Err(PreparedRuntimeError::Run(ExecutionError::NotQuiescent)));
    assert_eq!(uncertain.stage(), PreparedFailureStage::Install);
    assert_eq!(uncertain.kind(), PreparedFailureKind::Integrity);
    let source = uncertain
        .source()
        .unwrap()
        .downcast_ref::<PreparedRuntimeError>()
        .unwrap();
    assert!(matches!(source, PreparedRuntimeError::SourceScopeAdmission));
    match uncertain {
        PreparedRuntimeError::InstallCleanup { primary, cleanup } => {
            assert!(matches!(
                *primary,
                PreparedRuntimeError::SourceScopeAdmission
            ));
            assert!(matches!(
                *cleanup,
                PreparedRuntimeError::Run(ExecutionError::NotQuiescent)
            ));
            assert_eq!(cleanup.stage(), PreparedFailureStage::Run);
        }
        other => panic!("missing cleanup uncertainty: {other:?}"),
    }
}
