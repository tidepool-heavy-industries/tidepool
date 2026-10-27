use super::*;
use crate::session::{
    NotebookGenerationId, RuntimeSourceRetirementProof, SupersededCompilerProgress,
    SupersededPrivateExecution,
};
use tidepool_toolchain::compiler_world::PrivateCheckedItemProgress;

fn session(id: u64) -> (tempfile::TempDir, Arc<InputRecipe>, TestSession) {
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(&[]).unwrap();
    let recipe = Arc::new(InputRecipe::capture(
        effects.preamble().to_owned(),
        effects.row().to_owned(),
        effects.include_paths().to_vec(),
    ));
    let root = tempfile::tempdir().unwrap();
    let library = SessionLib::open(SessionId(id), root.path(), ModuleEnv::standalone_default())
        .unwrap()
        .with_validation_include(recipe.include.clone());
    let mut resident = TestSession::unbootstrapped(
        frunk::HNil,
        EmptyOutput,
        crate::DEFAULT_NURSERY_SIZE,
        Some(library),
    );
    resident.bootstrap_world(&recipe);
    (root, recipe, resident)
}

fn retire(resident: &TestSession) -> RuntimeSourceRetirementProof {
    resident
        .public_source
        .original_source()
        .source_admission()
        .fence()
        .unwrap()
        .retire(NotebookGenerationId::INITIAL.next().unwrap())
        .unwrap_or_else(|(_, error)| panic!("{error:?}"))
}

#[test]
fn retired_source_settles_only_its_exact_empty_execution_once() {
    let (_root, _recipe, mut resident) = session(1880);
    let execution = begin_fixture_execution(&mut resident);
    let before = resident.public_visibility_snapshot_in(resident.public_scope());
    let (_foreign_root, _, mut foreign) = session(1881);
    let foreign_proof = retire(&foreign);
    assert!(foreign
        .settle_superseded_private_execution(&execution, &foreign_proof)
        .is_err());
    let other_source = resident.mint_notebook_scope_with_lease(None).unwrap();
    let other_proof = other_source
        .source_admission()
        .fence()
        .unwrap()
        .retire(NotebookGenerationId::INITIAL.next().unwrap())
        .unwrap_or_else(|(_, error)| panic!("{error:?}"));
    assert!(resident
        .settle_superseded_private_execution(&execution, &other_proof)
        .is_err());
    assert!(resident
        .settle_superseded_private_execution(&execution, &foreign_proof)
        .is_err());
    assert!(resident
        .state
        .scope_tree()
        .is_live(execution.private_scope()));
    let proof = retire(&resident);
    let roots_before = resident.state.persistent_roots_count();
    let outcome = resident
        .settle_superseded_private_execution(&execution, &proof)
        .unwrap();
    let SupersededPrivateExecution::Empty { retirement } = outcome else {
        panic!("empty execution acquired writes")
    };
    assert_eq!(retirement.scopes_retired, 1);
    assert_eq!(
        roots_before - resident.state.persistent_roots_count(),
        retirement.roots_released
    );
    assert_eq!(retirement.bindings_retired, 0);
    assert!(!resident
        .state
        .scope_tree()
        .is_live(execution.private_scope()));
    assert_eq!(
        resident.public_visibility_snapshot_in(resident.public_scope()),
        before
    );
    assert!(resident
        .settle_superseded_private_execution(&execution, &proof)
        .is_err());
    assert!(resident.freeze_private_execution(&execution).is_err());
    assert!(resident
        .public_source
        .original_source()
        .source_admission()
        .fence()
        .is_err());
}

#[test]
fn retired_source_discards_completed_value_without_publishing() {
    let (_root, recipe, mut resident) = session(1882);
    let execution = begin_fixture_execution(&mut resident);
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..resident.public_context()
        })
        .unwrap();
    let (bound, compiled, item) = compile_checked_binding(
        &mut resident,
        &recipe,
        "discardedValue <- pure (41 :: Int)",
        execution.clone(),
    );
    let outcome = source_action(&mut resident, |resident, settlement| {
        resident.run_bind_with_sites(
            "discardedValue",
            compiled.code(),
            &bound[0],
            item.generation(),
            settlement,
        )
    })
    .unwrap();
    assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
    let before = resident.public_visibility_snapshot_in(resident.public_scope());
    let proof = retire(&resident);
    let roots_before = resident.state.persistent_roots_count();
    let outcome = resident
        .settle_superseded_private_execution(&execution, &proof)
        .unwrap();
    let SupersededPrivateExecution::Discarded { writes, retirement } = outcome else {
        panic!("completed value classified empty")
    };
    assert_eq!(writes.bindings.len(), 1);
    assert_eq!(writes.bindings[0].name, "discardedValue");
    assert_eq!(
        writes.compiler_progress,
        SupersededCompilerProgress::Checked
    );
    assert_eq!(writes.accepted_host_mounts, 0);
    assert_eq!(retirement.scopes_retired, 1);
    assert_eq!(
        roots_before - resident.state.persistent_roots_count(),
        retirement.roots_released
    );
    assert!(execution.completed_values.lock().is_empty());
    assert_eq!(
        resident.public_visibility_snapshot_in(resident.public_scope()),
        before
    );
    assert!(resident
        .state
        .resolve_in(resident.public_scope(), "discardedValue")
        .is_none());
}

#[test]
fn retired_source_refuses_active_execution_without_mutation() {
    let (_root, recipe, mut resident) = session(1883);
    let execution = begin_fixture_execution(&mut resident);
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..resident.public_context()
        })
        .unwrap();
    let checked = check_fixture_cell(
        &mut resident,
        &recipe,
        "stillActive <- pure (41 :: Int)",
        execution.clone(),
        0,
    );
    let (_bound, compiled, _item) = checked.compile_binding(&mut resident, 0);
    let native = compiled
        .certification
        .as_ref()
        .unwrap()
        .checked_execution()
        .unwrap()
        .clone();
    let running = source_action(&mut resident, |resident, _| {
        checked
            .prefix
            .start(&resident.state, execution.private_scope(), native)
    })
    .unwrap();
    let before = resident.public_visibility_snapshot_in(execution.private_scope());
    let proof = retire(&resident);
    assert!(matches!(
        resident.settle_superseded_private_execution(&execution, &proof),
        Err(SessionError::WorldCellPublicationIncomplete)
    ));
    assert_eq!(
        resident.public_visibility_snapshot_in(execution.private_scope()),
        before
    );
    assert!(execution.final_intent.get().is_none());
    assert!(resident
        .state
        .scope_tree()
        .is_live(execution.private_scope()));
    assert_eq!(
        execution.checked_progress(),
        PrivateCheckedItemProgress::Started
    );
    running
        .completion()
        .park(execution.private_scope())
        .unwrap();
    assert!(matches!(
        resident.settle_superseded_private_execution(&execution, &proof),
        Err(SessionError::WorldCellPublicationIncomplete)
    ));
    running
        .completion()
        .begin_reentry(&resident.state, execution.private_scope())
        .unwrap();
    let capture = running.completion().capture_ticket().unwrap();
    assert!(matches!(
        resident.settle_superseded_private_execution(&execution, &proof),
        Err(SessionError::WorldCellPublicationIncomplete)
    ));
    drop(capture);
    let settled = resident
        .settle_superseded_private_execution(&execution, &proof)
        .unwrap();
    assert!(
        matches!(settled, SupersededPrivateExecution::Discarded { writes, .. } if writes.compiler_progress == SupersededCompilerProgress::Incomplete)
    );
    assert_eq!(
        execution.checked_progress(),
        PrivateCheckedItemProgress::Started
    );
    assert_eq!(checked.prefix.snapshot().compiler_prefix().next_item(), 0);
    assert!(running.completion().capture_ticket().is_err());
    drop(running);
}

#[test]
fn retired_source_discards_declaration_without_publishing() {
    let (_root, recipe, mut resident) = session(1884);
    let execution = begin_fixture_execution(&mut resident);
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..resident.public_context()
        })
        .unwrap();
    let cell = check_fixture_cell(
        &mut resident,
        &recipe,
        "data DiscardedFlag = DiscardedFlag",
        execution.clone(),
        1,
    );
    cell.adopt_declaration(&mut resident);
    let before = resident.public_visibility_snapshot_in(resident.public_scope());
    let proof = retire(&resident);
    let roots_before = resident.state.persistent_roots_count();
    let outcome = resident
        .settle_superseded_private_execution(&execution, &proof)
        .unwrap();
    let SupersededPrivateExecution::Discarded { writes, retirement } = outcome else {
        panic!("declaration classified empty")
    };
    assert_eq!(
        roots_before - resident.state.persistent_roots_count(),
        retirement.roots_released
    );
    assert!(writes.declarations);
    assert!(writes.bindings.is_empty());
    assert_eq!(
        writes.compiler_progress,
        SupersededCompilerProgress::Checked
    );
    assert_eq!(retirement.scopes_retired, 1);
    assert_eq!(
        resident.public_visibility_snapshot_in(resident.public_scope()),
        before
    );
}

#[test]
fn retired_source_refuses_pending_hosts_and_discards_accepted_hosts_without_reconciliation() {
    for (index, accept, reconcile) in [(0, false, false), (1, true, false), (2, true, true)] {
        let (_root, recipe, mut resident) = session(1885 + index);
        let execution = begin_fixture_execution(&mut resident);
        resident
            .set_run_context(SessionRunContext {
                lexical_scope: execution.private_scope(),
                ..resident.public_context()
            })
            .unwrap();
        let carrier =
            checked_text_host_carrier(&mut resident, &recipe, &execution, "discardedHost");
        let mut pending = source_action(&mut resident, |resident, settlement| {
            resident.mount_private_visible_host_input(
                &execution,
                &carrier,
                HostPayload::Text("value"),
                settlement,
            )
        })
        .unwrap();
        if accept {
            pending.accept().unwrap();
        }
        if reconcile {
            source_action(&mut resident, |resident, _| {
                resident.prepare_private_host_carrier(execution.clone(), [19; 32])
            })
            .unwrap();
        }
        let public_before = resident.public_visibility_snapshot_in(resident.public_scope());
        let private_before = resident.public_visibility_snapshot_in(execution.private_scope());
        let other_public = resident.mint_notebook_scope_with_lease(None).unwrap();
        let other_before = resident.public_visibility_snapshot_in(other_public.scope());
        let proof = retire(&resident);
        let roots_before = resident.state.persistent_roots_count();
        let result = resident.settle_superseded_private_execution(&execution, &proof);
        if accept {
            let SupersededPrivateExecution::Discarded { writes, retirement } = result.unwrap()
            else {
                panic!("host classified empty")
            };
            assert_eq!(writes.accepted_host_mounts, 1);
            assert_eq!(writes.bindings[0].name, "discardedHost");
            assert_eq!(retirement.scopes_retired, 1);
            assert_eq!(
                roots_before - resident.state.persistent_roots_count(),
                retirement.roots_released
            );
        } else {
            assert!(matches!(
                result,
                Err(SessionError::WorldCellPublicationIncomplete)
            ));
            assert_eq!(
                resident.public_visibility_snapshot_in(execution.private_scope()),
                private_before
            );
            assert!(resident
                .state
                .scope_tree()
                .is_live(execution.private_scope()));
        }
        assert_eq!(
            resident.public_visibility_snapshot_in(other_public.scope()),
            other_before
        );
        assert!(resident.public_source.source_admission().admit().is_err());
        assert_eq!(
            resident.public_visibility_snapshot_in(resident.public_scope()),
            public_before
        );
    }
}

#[test]
fn retired_source_refuses_stale_epoch_without_mutation() {
    let (_root, _, mut resident) = session(1888);
    let execution = begin_fixture_execution(&mut resident);
    let proof = retire(&resident);
    let before = resident.public_visibility_snapshot_in(execution.private_scope());
    let next = resident
        .state
        .prepare_execution_admission_epoch_advance()
        .unwrap();
    resident
        .state
        .invalidate_execution_admissions_after_owner_transfer(next);
    assert!(matches!(
        resident.settle_superseded_private_execution(&execution, &proof),
        Err(SessionError::TransitionRefused(
            crate::session::SessionTransitionReason::OwnerChanged
        ))
    ));
    assert_eq!(
        resident.public_visibility_snapshot_in(execution.private_scope()),
        before
    );
    assert!(resident
        .state
        .scope_tree()
        .is_live(execution.private_scope()));
    assert!(execution.final_intent.get().is_none());
}

#[test]
fn retired_source_aborts_reserved_prefix_without_completing_it() {
    let (_root, recipe, mut resident) = session(1889);
    let execution = begin_fixture_execution(&mut resident);
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..resident.public_context()
        })
        .unwrap();
    let checked = check_fixture_cell(
        &mut resident,
        &recipe,
        "unstarted <- pure (41 :: Int)",
        execution.clone(),
        0,
    );
    let (_, compiled, _) = checked.compile_binding(&mut resident, 0);
    let native = compiled
        .certification
        .as_ref()
        .unwrap()
        .checked_execution()
        .unwrap()
        .clone();
    let proof = retire(&resident);
    let public_before = resident.public_visibility_snapshot_in(resident.public_scope());
    let settled = resident
        .settle_superseded_private_execution(&execution, &proof)
        .unwrap();
    let SupersededPrivateExecution::Discarded { writes, retirement } = settled else {
        panic!("reserved program cannot claim empty completed frontier")
    };
    assert_eq!(
        writes.compiler_progress,
        SupersededCompilerProgress::Incomplete
    );
    assert!(writes.bindings.is_empty());
    assert_eq!(
        execution.checked_progress(),
        PrivateCheckedItemProgress::Started
    );
    assert_eq!(checked.prefix.snapshot().compiler_prefix().next_item(), 0);
    assert_eq!(retirement.scopes_retired, 1);
    assert!(checked
        .prefix
        .start(&resident.state, execution.private_scope(), native)
        .is_err());
    assert!(resident.freeze_private_execution(&execution).is_err());
    assert_eq!(
        resident.public_visibility_snapshot_in(resident.public_scope()),
        public_before
    );
}

#[test]
fn retired_source_discards_failed_and_cancelled_suffixes_without_losing_prior_writes() {
    for cancel in [false, true] {
        let (_root, recipe, mut resident) = session(1890 + u64::from(cancel));
        let execution = begin_fixture_execution(&mut resident);
        resident
            .set_run_context(SessionRunContext {
                lexical_scope: execution.private_scope(),
                ..resident.public_context()
            })
            .unwrap();
        let source = if cancel {
            "prior <- pure (41 :: Int)\nsuffix <- pure (42 :: Int)"
        } else {
            "prior <- pure (41 :: Int)\nsuffix <- (error \"superseded suffix\" >> pure (42 :: Int))"
        };
        let checked = check_fixture_cell(&mut resident, &recipe, source, execution.clone(), 0);
        let (bound, compiled, item) = checked.compile_binding(&mut resident, 0);
        let first = source_action(&mut resident, |resident, settlement| {
            resident.run_bind_with_sites(
                "prior",
                compiled.code(),
                &bound[0],
                item.generation(),
                settlement,
            )
        })
        .unwrap();
        assert!(matches!(first, ResidentOutcome::Completed { .. }));
        let (bound, compiled, item) = checked.compile_binding(&mut resident, 1);
        let failed = source_action(&mut resident, |resident, settlement| {
            resident.with_invocation_cancel(
                Arc::new(std::sync::atomic::AtomicBool::new(cancel)),
                |resident| {
                    resident.run_bind_with_sites(
                        "suffix",
                        compiled.code(),
                        &bound[0],
                        item.generation(),
                        settlement,
                    )
                },
            )
        });
        let error = failed.expect_err("the suffix must actually fail or observe cancellation");
        if cancel {
            assert!(
                matches!(&error, ResidentError::Prepared(prepared) if prepared.kind() == crate::session::prepared::PreparedFailureKind::Cancelled),
                "{error:?}"
            );
        } else {
            assert!(error.is_language_failure(), "{error:?}");
        }
        let proof = retire(&resident);
        let before = resident.public_visibility_snapshot_in(resident.public_scope());
        let settled = resident
            .settle_superseded_private_execution(&execution, &proof)
            .unwrap();
        let SupersededPrivateExecution::Discarded { writes, retirement } = settled else {
            panic!("failed suffix lost prior staged value")
        };
        assert_eq!(
            writes.compiler_progress,
            SupersededCompilerProgress::Incomplete
        );
        assert_eq!(
            writes
                .bindings
                .iter()
                .map(|binding| binding.name.as_str())
                .collect::<Vec<_>>(),
            ["prior"]
        );
        assert_eq!(checked.prefix.snapshot().compiler_prefix().next_item(), 1);
        assert_eq!(
            execution.checked_progress(),
            PrivateCheckedItemProgress::Started
        );
        assert_eq!(retirement.scopes_retired, 1);
        assert!(execution.completed_values.lock().is_empty());
        assert_eq!(
            resident.public_visibility_snapshot_in(resident.public_scope()),
            before
        );
        assert!(resident.freeze_private_execution(&execution).is_err());
        assert!(resident
            .settle_superseded_private_execution(&execution, &proof)
            .is_err());
    }
}
