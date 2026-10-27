use super::*;
use crate::session::{
    NotebookGenerationId, RuntimeSourceRetirementProof, SupersededCompilerProgress,
    SupersededPrivateExecution,
};
use tidepool_toolchain::compiler_world::PrivateCheckedItemProgress;

fn session(id: u64) -> (tempfile::TempDir, Arc<InputRecipe>, TestSession) {
    session_with_effects(id, &[])
}

fn session_with_effects(
    id: u64,
    declarations: &[tidepool_mcp::EffectDecl],
) -> (tempfile::TempDir, Arc<InputRecipe>, TestSession) {
    tidepool_testing::eval_harness::require_extract();
    let effects = TestEffectSurface::minimal(declarations).unwrap();
    let recipe = Arc::new(InputRecipe::capture(
        if declarations.is_empty() {
            effects.preamble().to_owned()
        } else {
            insert_preamble_imports(effects.preamble(), "Tidepool.Duration (milliseconds)")
        },
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

fn start_parked_return(
    resident: &mut TestSession,
    recipe: &InputRecipe,
    execution: Arc<crate::session::PrivateExecutionAdmission>,
    source: &str,
) -> (CheckedFixtureCell, ResidentHole) {
    resident
        .set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..resident.public_context()
        })
        .unwrap();
    let checked = check_fixture_cell(resident, recipe, source, execution, 0);
    let item = checked.checked.checked_item(0).unwrap();
    let admission = source_action(resident, |resident, _| {
        resident.admit_checked_item(checked.prefix.clone(), item)
    })
    .unwrap();
    let turn = turn::consume_cell_program_item(admission.clone()).unwrap();
    let outcome = source_action(resident, |resident, settlement| match &turn {
        TurnResult::Expr { compiled, .. } => {
            resident.run_with_sites("retiredReturn", compiled.code(), settlement)
        }
        TurnResult::Bind {
            bound, compiled, ..
        } if bound.is_empty() => {
            resident.run_with_sites("retiredReturn", compiled.code(), settlement)
        }
        TurnResult::Bind {
            bound, compiled, ..
        } if bound.len() == 1 => resident.run_bind_with_sites(
            "retiredReturn",
            compiled.code(),
            &bound[0],
            admission.generation(),
            settlement,
        ),
        TurnResult::Bind {
            bound, compiled, ..
        } => resident.run_projected_bind_with_sites(
            "retiredReturn",
            compiled.code(),
            bound,
            admission.generation(),
            settlement,
        ),
        TurnResult::Decl(_) => panic!("return fixture must execute"),
    })
    .unwrap();
    (checked, suspended(outcome))
}

fn retire_return(
    resident: &TestSession,
    owner: crate::session::RuntimeCheckedReturnOwner,
) -> RuntimeSourceRetirementProof {
    resident
        .public_source
        .original_source()
        .source_admission()
        .fence()
        .unwrap()
        .retire_with_checked_return(NotebookGenerationId::INITIAL.next().unwrap(), Some(owner))
        .unwrap_or_else(|(_, error)| panic!("{error:?}"))
}

#[test]
fn retired_checked_returns_preserve_results_and_discard_unpublished_captures() {
    let cases = [
        ("sleep (milliseconds 1) >> pure (41 :: Int)", vec![]),
        ("returnedValue <- sleep (milliseconds 1) >> pure (41 :: Int)", vec!["returnedValue"]),
        ("(returnedLeft, returnedRight) <- sleep (milliseconds 1) >> pure (41 :: Int, 42 :: Int)", vec!["returnedLeft", "returnedRight"]),
        ("_ <- sleep (milliseconds 1)", vec![]),
        ("observation0 <- sleep (milliseconds 1) >> pure (41 :: Int)", vec!["observation0"]),
    ];
    for (index, (source, expected_names)) in cases.into_iter().enumerate() {
        let (_root, recipe, mut resident) =
            session_with_effects(1900 + index as u64, &[tidepool_mcp::sleep_decl()]);
        let execution = begin_fixture_execution(&mut resident);
        let (checked, hole) =
            start_parked_return(&mut resident, &recipe, execution.clone(), source);
        let successor = resident.mint_notebook_scope_with_lease(None).unwrap();
        let before = resident.public_visibility_snapshot_in(resident.public_scope());
        let successor_before = resident.public_visibility_snapshot_in(successor.scope());
        let owner = resident
            .capture_checked_return_owner(&execution, &hole)
            .unwrap()
            .unwrap();
        let proof = retire_return(&resident, owner);
        assert!(resident
            .capture_checked_return_owner(&execution, &hole)
            .is_err());
        let outcome = resident
            .resume_superseded_classified(hole, (), &proof, &mut |_| {})
            .unwrap();
        let ResidentOutcome::SupersededReturn {
            bindings, result, ..
        } = outcome
        else {
            panic!("retired return reported ordinary publication")
        };
        assert_eq!(
            bindings
                .iter()
                .map(|binding| binding.name.as_str())
                .collect::<Vec<_>>(),
            expected_names
        );
        assert_eq!(result.is_some(), index != 2);
        if index == 0 || index == 1 || index == 4 {
            assert_eq!(result.as_ref().unwrap().to_json(), serde_json::json!(41));
        }
        assert_eq!(checked.prefix.snapshot().compiler_prefix().next_item(), 0);
        assert_eq!(
            execution.checked_progress(),
            PrivateCheckedItemProgress::Started
        );
        assert!(execution.completed_values.lock().is_empty());
        assert!(resident.parked_holes().is_empty());
        assert_eq!(resident.stowed_roots_count(), 0);
        assert!(resident.custody_cleanup.abandoned.lock().is_empty());
        assert_eq!(resident.outstanding_custody(), 0);
        for name in &expected_names {
            assert!(resident
                .state
                .resolve_in(execution.private_scope(), name)
                .is_none());
        }
        assert_eq!(
            resident.public_visibility_snapshot_in(resident.public_scope()),
            before
        );
        assert_eq!(
            resident.public_visibility_snapshot_in(successor.scope()),
            successor_before
        );
        assert!(resident.public_source.admit_source().is_err());
        assert!(resident.freeze_private_execution(&execution).is_err());
        let settled = resident
            .settle_superseded_private_execution(&execution, &proof)
            .unwrap();
        match settled {
            SupersededPrivateExecution::Empty { .. } => assert!(expected_names.is_empty()),
            SupersededPrivateExecution::Discarded { writes, .. } => {
                assert!(!expected_names.is_empty());
                assert_eq!(
                    writes
                        .bindings
                        .iter()
                        .map(|binding| binding.name.as_str())
                        .collect::<Vec<_>>(),
                    expected_names
                );
                assert_eq!(
                    writes.compiler_progress,
                    SupersededCompilerProgress::Incomplete
                );
            }
        }
        assert_eq!(
            resident.public_visibility_snapshot_in(resident.public_scope()),
            before
        );
        assert_eq!(
            resident.public_visibility_snapshot_in(successor.scope()),
            successor_before
        );
    }
}

#[test]
fn retired_checked_return_refuses_sibling_and_unknown_holes_without_consuming() {
    let (_root, recipe, mut resident) = session_with_effects(1904, &[tidepool_mcp::sleep_decl()]);
    let first = begin_fixture_execution(&mut resident);
    let (_, first_hole) = start_parked_return(
        &mut resident,
        &recipe,
        first.clone(),
        "sleep (milliseconds 1)",
    );
    let owner = resident
        .capture_checked_return_owner(&first, &first_hole)
        .unwrap()
        .unwrap();
    let sibling = begin_fixture_execution(&mut resident);
    let (_, sibling_hole) = start_parked_return(
        &mut resident,
        &recipe,
        sibling.clone(),
        "sleep (milliseconds 1)",
    );
    assert!(resident
        .capture_checked_return_owner(&sibling, &first_hole)
        .is_err());
    let proof = retire_return(&resident, owner);
    let before = resident
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for hole in [
        sibling_hole.clone(),
        ResidentHole::plain("not-an-issued-hole".to_owned()),
    ] {
        assert!(matches!(
            resident.resume_superseded_classified(hole, (), &proof, &mut |_| {}),
            Err(ResidentResumeError::Rejected(_))
        ));
        assert_eq!(
            resident.parked_holes(),
            before.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }
    assert!(matches!(
        resident
            .resume_superseded_classified(first_hole, (), &proof, &mut |_| {})
            .unwrap(),
        ResidentOutcome::SupersededReturn { .. }
    ));
    assert!(
        resident
            .resume_classified(sibling_hole, (), &mut |_| {})
            .is_err(),
        "ordinary sibling capture still needs open source"
    );
    resident
        .settle_superseded_private_execution(&first, &proof)
        .unwrap();
    assert!(matches!(
        resident
            .settle_superseded_private_execution(&sibling, &proof)
            .unwrap(),
        SupersededPrivateExecution::Discarded { .. }
    ));
}

#[test]
fn source_retirement_without_selected_return_does_not_authorize_capture() {
    let (_root, recipe, mut resident) = session_with_effects(1905, &[tidepool_mcp::sleep_decl()]);
    let execution = begin_fixture_execution(&mut resident);
    let (_, hole) = start_parked_return(
        &mut resident,
        &recipe,
        execution.clone(),
        "sleep (milliseconds 1)",
    );
    let proof = retire(&resident);
    assert!(matches!(
        resident.resume_superseded_classified(hole.clone(), (), &proof, &mut |_| {}),
        Err(ResidentResumeError::Rejected(_))
    ));
    assert_eq!(resident.parked_holes(), vec![hole.cont_id()]);
    assert!(resident.resume_classified(hole, (), &mut |_| {}).is_err());
    assert!(matches!(
        resident
            .settle_superseded_private_execution(&execution, &proof)
            .unwrap(),
        SupersededPrivateExecution::Discarded { .. }
    ));
}

#[test]
fn selected_return_survives_same_item_reparking_without_authorizing_tail() {
    let (_root, recipe, mut resident) = session_with_effects(1906, &[tidepool_mcp::sleep_decl()]);
    let execution = begin_fixture_execution(&mut resident);
    let (checked, hole) = start_parked_return(
        &mut resident,
        &recipe,
        execution.clone(),
        "sleep (milliseconds 1) >> sleep (milliseconds 2)\nlet forbiddenTail = 9 :: Int",
    );
    let owner = resident
        .capture_checked_return_owner(&execution, &hole)
        .unwrap()
        .unwrap();
    let proof = retire_return(&resident, owner);
    let second = suspended(
        resident
            .resume_superseded_classified(hole, (), &proof, &mut |_| {})
            .unwrap(),
    );
    assert!(
        matches!(resident.resume_classified(second, (), &mut |_| {}).unwrap(), ResidentOutcome::SupersededReturn { bindings, .. } if bindings.is_empty())
    );
    assert_eq!(checked.prefix.snapshot().compiler_prefix().next_item(), 0);
    assert!(resident
        .admit_checked_item(
            checked.prefix.clone(),
            checked.checked.checked_item(1).unwrap()
        )
        .is_err());
    assert!(resident
        .state
        .resolve_in(execution.private_scope(), "forbiddenTail")
        .is_none());
    assert!(
        matches!(resident.settle_superseded_private_execution(&execution, &proof).unwrap(), SupersededPrivateExecution::Discarded { writes, .. } if writes.bindings.is_empty() && writes.compiler_progress == SupersededCompilerProgress::Incomplete)
    );
}

#[test]
fn refused_source_retirement_cannot_seal_selected_return() {
    let (_root, recipe, mut resident) = session_with_effects(1907, &[tidepool_mcp::sleep_decl()]);
    let execution = begin_fixture_execution(&mut resident);
    let (_, hole) = start_parked_return(
        &mut resident,
        &recipe,
        execution.clone(),
        "sleep (milliseconds 1)",
    );
    let selected = resident
        .capture_checked_return_owner(&execution, &hole)
        .unwrap()
        .unwrap();
    let original = resident.public_source.original_source().clone();
    let claim = original.admit_source().unwrap();
    let fence = original.source_admission().fence().unwrap();
    let (fence, _) = fence
        .retire_with_checked_return(
            NotebookGenerationId::INITIAL.next().unwrap(),
            Some(selected),
        )
        .expect_err("live admitted source work prevents retirement");
    drop(claim);
    fence.reopen(&original).unwrap();
    assert!(matches!(
        source_action(&mut resident, |resident, settlement| resident
            .resume_classified(hole, (), settlement))
        .unwrap(),
        ResidentOutcome::Completed { .. }
    ));
    assert_eq!(
        execution.checked_progress(),
        PrivateCheckedItemProgress::Completed
    );
}
