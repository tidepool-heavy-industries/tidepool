use super::*;

fn receipt() -> WorkbenchItemReceipt {
    WorkbenchItemReceipt {
        presentation: None,
        index: 0,
        kind: Some(WorkbenchCellItemKind::Statement),
        span: None,
        source_items: Vec::new(),
        status: WorkbenchItemStatus::Committed,
        output: String::new(),
        value: None,
        diagnostics: Vec::new(),
        failure_layer: None,
        warnings: Vec::new(),
        installed_bindings: vec!["privateProgress".into()],
        operations: Vec::new(),
        terminal_transfer: None,
    }
}

fn response(status: WorkbenchRunStatus) -> WorkbenchResponse {
    WorkbenchResponse {
        status,
        publication: None,
        summary: None,
        items: vec![receipt()],
        next_index: 1,
        total: 2,
    }
}

#[test]
fn unsuccessful_cells_never_publish_private_progress() {
    for (status, reason) in [
        (
            WorkbenchRunStatus::Rejected,
            WorkbenchNotPublishedReason::Rejected,
        ),
        (
            WorkbenchRunStatus::RequestCancelled,
            WorkbenchNotPublishedReason::Cancelled,
        ),
    ] {
        let mut result = Ok(KernelStep::Continue(response(status)));
        assert_eq!(private_nonpublication_reason(&result), Some(reason));
        set_private_publication(
            &mut result,
            WorkbenchPublicationOutcome::NotPublished { reason },
        );
        let Ok(KernelStep::Continue(response)) = result else {
            unreachable!()
        };
        assert!(response.publication.unwrap().public_bindings().is_empty());
        assert_eq!(response.items[0].installed_bindings, ["privateProgress"]);
    }
    let mut failed = Err(WorkbenchExecutionFailure {
        receipts: vec![receipt()],
        point: WorkbenchFailurePoint::InputUnit { index: 1 },
        publication: None,
        total: 2,
        source: ResidentActorWorkbenchError::ActorProtocol("effect failed after binding".into()),
    });
    assert_eq!(
        private_nonpublication_reason(&failed),
        Some(WorkbenchNotPublishedReason::Failed)
    );
    set_private_publication(
        &mut failed,
        WorkbenchPublicationOutcome::NotPublished {
            reason: WorkbenchNotPublishedReason::Failed,
        },
    );
    let failure = failed.unwrap_err();
    assert!(failure.publication.unwrap().public_bindings().is_empty());
    assert_eq!(failure.receipts[0].installed_bindings, ["privateProgress"]);
    assert!(failure
        .source
        .to_string()
        .contains("effect failed after binding"));
}

#[test]
fn successful_terminal_transfers_publish_the_executed_cell_and_skip_the_suffix() {
    for status in [
        WorkbenchRunStatus::Committed,
        WorkbenchRunStatus::Completed,
        WorkbenchRunStatus::Replied,
        WorkbenchRunStatus::Backgrounded,
    ] {
        let mut response = response(status);
        if matches!(
            status,
            WorkbenchRunStatus::Committed | WorkbenchRunStatus::Completed
        ) {
            response.total = response.next_index;
        }
        let expected_total = response.total;
        let result = Ok(KernelStep::Continue(response));
        assert_eq!(private_nonpublication_reason(&result), None);
        let result = mark_private_publication(result, &["acceptedHostWrite".into()]);
        let Ok(KernelStep::Continue(response)) = result else {
            unreachable!()
        };
        assert_eq!(response.next_index, 1);
        assert_eq!(response.total, expected_total);
        assert_eq!(
            response.publication.unwrap().public_bindings(),
            ["acceptedHostWrite", "privateProgress"]
        );
    }
}

#[test]
fn cleanup_uncertainty_and_terminal_wrappers_preserve_publication_and_receipts() {
    let actor = ActorRef::first(crate::ActorId(1));
    for publication in [
        WorkbenchPublicationOutcome::Published {
            bindings: vec!["visible".into()],
        },
        WorkbenchPublicationOutcome::RecoveryRequired {
            detail: "database commit outcome requires recovery".into(),
        },
        WorkbenchPublicationOutcome::NotPublished {
            reason: WorkbenchNotPublishedReason::Failed,
        },
    ] {
        let mut response = response(WorkbenchRunStatus::Committed);
        response.publication = Some(publication.clone());
        let failed = retain_cleanup_failure(
            actor,
            "model cleanup missing".into(),
            WorkbenchFinalizationResult {
                result: Ok(KernelStep::Continue(response)),
                cleanup_confirmed: true,
            },
        );
        assert!(!failed.cleanup_confirmed);
        let failure = failed.result.unwrap_err();
        assert_eq!(failure.publication(), Some(&publication));
        assert_eq!(failure.receipts().len(), 1);
        let wrapped = KernelInvocationFailure::TerminalTransferFailed {
            actor,
            request: crate::RequestId(1),
            source: Box::new(failure),
        };
        let failed = retain_cleanup_failure(
            actor,
            "second cleanup missing".into(),
            WorkbenchFinalizationResult {
                result: Err(wrapped),
                cleanup_confirmed: false,
            },
        );
        let failure = failed.result.unwrap_err();
        assert_eq!(failure.publication(), Some(&publication));
        assert_eq!(failure.receipts().len(), 1);
    }
}

#[test]
fn an_incomplete_ordinary_return_cannot_publish_a_cell() {
    let result = Ok(KernelStep::Continue(response(
        WorkbenchRunStatus::Committed,
    )));
    assert_eq!(
        private_nonpublication_reason(&result),
        Some(WorkbenchNotPublishedReason::Failed)
    );
}

#[test]
fn publication_error_obeys_the_cancellation_arbiter_and_retains_diagnostics() {
    for cancellation_wins in [true, false] {
        let control = crate::WorkbenchExecutionControl::untracked();
        if cancellation_wins {
            assert!(control.request_cancellation());
        } else {
            let claim = control.publication_decision().claim_commit().unwrap();
            assert!(!control.request_cancellation());
            assert!(claim.before_rename_failure());
        }
        let failure = private_publication_rejection(
            &control,
            Ok(KernelStep::Continue(response(
                WorkbenchRunStatus::Committed,
            ))),
            ResidentActorWorkbenchError::ActorProtocol("publication preparation failed".into()),
        );
        let expected = if cancellation_wins {
            WorkbenchPublicationOutcome::NotPublished {
                reason: WorkbenchNotPublishedReason::Cancelled,
            }
        } else {
            WorkbenchPublicationOutcome::Rejected {
                detail: failure.source.to_string(),
            }
        };
        assert_eq!(failure.publication.as_ref(), Some(&expected));
        assert!(expected.public_bindings().is_empty());
        assert_eq!(failure.receipts.len(), 1);
        assert_eq!(failure.receipts[0].installed_bindings, ["privateProgress"]);
        assert!(failure
            .source
            .to_string()
            .contains("publication preparation failed"));
        let actor = ActorRef::first(crate::ActorId(1));
        let result = Err(KernelInvocationFailure::Workbench(
            crate::KernelWorkbenchFailure {
                execution_failure_kind: None,
                actor,
                receipts: failure.receipts,
                point: failure.point,
                total: failure.total,
                publication: failure.publication,
                diagnostic: failure.source.failure_diagnostic(),
                detail: failure.source.to_string(),
            },
        ));
        let exit = control.finish_cell(WorkbenchExecutionId::from_digest([30; 16]), &result, true);
        assert_eq!(
            exit.cause,
            if cancellation_wins {
                crate::CellExitCause::Cancelled
            } else {
                crate::CellExitCause::Failed
            }
        );
    }
}

#[test]
fn publication_error_termination_prevents_a_later_cancellation_from_reclassifying_it() {
    let control = crate::WorkbenchExecutionControl::untracked();
    let failure = private_publication_rejection(
        &control,
        Ok(KernelStep::Continue(response(
            WorkbenchRunStatus::Committed,
        ))),
        ResidentActorWorkbenchError::ActorProtocol("certification refused".into()),
    );
    assert!(!control.request_cancellation());
    assert!(control.context_cancellation_requested());
    assert!(matches!(
        failure.publication,
        Some(WorkbenchPublicationOutcome::Rejected { .. })
    ));
    assert!(!control
        .native_cancel()
        .load(std::sync::atomic::Ordering::Acquire));
}

#[tokio::test]
async fn prepared_publication_observing_a_sibling_fence_requires_recovery() {
    crate::resident_workbench::with_test_compiler_owner(|client| {
        sibling_fence_before_preflight(false, client)
    })
    .await;
}

#[tokio::test]
async fn prepared_publication_observing_a_sibling_fence_preserves_recovery_during_cancellation() {
    crate::resident_workbench::with_test_compiler_owner(|client| {
        sibling_fence_before_preflight(true, client)
    })
    .await;
}

async fn sibling_fence_before_preflight(cancel: bool, client: crate::CompilerPreparationClient) {
    use crate::recovery::recovery_test_fixture::RecoveryFixture;
    use tidepool_runtime::session::{
        fresh_session_id, ModuleEnv, RecoveryDomainId, RecoveryPublicOwner, SessionError,
        SessionLib, StoreErrorKind,
    };

    let root = tempfile::tempdir().unwrap();
    let recovery = RecoveryFixture::create(root.path());
    let surface = tidepool_testing::effect_surface::TestEffectSurface::minimal(&[]).unwrap();
    let session_id = fresh_session_id();
    let mut lib = SessionLib::open(
        session_id,
        root.path().join("machine-a"),
        ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(surface.include_paths().to_vec());
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement));
    let mut session = tidepool_runtime::session::ResidentSession::unbootstrapped(
        frunk::HNil,
        tidepool_mcp::CapturedOutput::new(),
        1024,
        Some(lib),
    );
    let source_lease = session.mint_notebook_scope_with_lease(None).unwrap();
    let scope = source_lease.scope();
    let path = crate::ActorPath::parse("root/a").unwrap();
    let owner = RecoveryPublicOwner::new(&path, 1).unwrap();
    let descriptor = crate::ActorDescriptor::new(
        "sibling fence",
        crate::ActorPlacement {
            session: session_id,
            lexical_scope: scope,
            resource_scope: tidepool_codegen::suspension::RealmId::fresh(),
        },
    )
    .with_actor_path(path)
    .with_persistence_policy(crate::ActorPersistencePolicy::Durable);
    let mut context = descriptor
        .clone()
        .admit_independent_world()
        .session_context(crate::ActorRef::first(crate::ActorId(1)));
    let (source, _source_owner) =
        crate::resident_workbench::request_tests::world_query_test_source_files_with_support(
            &[],
            surface.include_paths(),
        );
    context.source_layer = crate::ActorSourceRoots::Complete(source);
    context.haskell_effects_alias = surface.row().into();
    assert!(matches!(
        session
            .initialize_durable_public_scope(owner.clone(), scope)
            .unwrap(),
        tidepool_runtime::session::PublicManifestCommit::Durable
    ));
    let confirmed = recovery.public_owner(&owner);
    let readiness = session.durable_public_readiness(&owner, scope).unwrap();
    let public_owner = WorkbenchPublicOwner::issue(&context, &descriptor, Some(readiness)).unwrap();
    let machines = Arc::new(ActorMachineRegistry::new());
    machines.insert_idle(session_id, Box::new(session));
    let runner = ResidentActorRunner::new(
        machines.clone(),
        ActorWorkbenchSource::new(surface.preamble(), surface.include_paths().to_vec())
            .bind_preparation(client),
    );
    runner
        .adopt_final_source_scope(context.clone())
        .await
        .unwrap();
    let admitted = runner
        .begin_private_execution(
            context.clone(),
            public_owner,
            tidepool_runtime::session::PublicationDecision::new(),
            None,
        )
        .await
        .unwrap();
    let (mut session, receipt) = machines.checkout_run(session_id).unwrap().into_parts();
    let prepared = session
        .freeze_private_execution(&admitted.admission)
        .unwrap();
    let before = session.public_visibility_snapshot_in(scope).unwrap();

    let sibling_id = fresh_session_id();
    let mut sibling = SessionLib::open(
        sibling_id,
        root.path().join("machine-b"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    tidepool_testing::with_settlement(|settlement| {
        recovery.attach(
            &mut sibling,
            RecoveryDomainId::Session(sibling_id),
            settlement,
        )
    });
    recovery.fail_commits();
    let failed = sibling
        .initialize_captured_declaration_high_water(tidepool_repr::Generation(1))
        .unwrap_err();
    assert!(matches!(
        failed,
        SessionError::RecoveryStore(ref error) if error.kind == StoreErrorKind::RecoveryRequired
    ));
    recovery.clear_commit_failure();
    assert!(recovery.store.is_recovery_required());

    let control = Arc::new(crate::WorkbenchExecutionControl::untracked());
    let preflight = std::thread::scope(|threads| {
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let cancellation = cancel.then(|| {
            let control = Arc::clone(&control);
            let barrier = Arc::clone(&barrier);
            threads.spawn(move || {
                barrier.wait();
                assert!(control.request_cancellation());
            })
        });
        if cancel {
            barrier.wait();
        }
        let preflight = session.restage_execution_publication(owner.clone(), prepared);
        if let Some(cancellation) = cancellation {
            cancellation.join().unwrap();
        }
        preflight
            .err()
            .expect("old prepared publication observes the sibling fence")
    });
    let error = ResidentActorWorkbenchError::PrivatePublication {
        actor: ActorRef::first(crate::ActorId(1)),
        scope,
        phase: crate::resident_workbench::PrivatePublicationPhase::Restage,
        source: Box::new(ResidentActorWorkbenchError::Resident(
            ResidentError::Session(preflight),
        )),
    };
    assert!(
        error.requires_recovery(),
        "preflight refused with {error:?}"
    );
    let failure = private_publication_rejection(
        &control,
        Ok(KernelStep::Continue(response(
            WorkbenchRunStatus::Committed,
        ))),
        error,
    );
    assert!(matches!(
        failure.publication,
        Some(WorkbenchPublicationOutcome::RecoveryRequired { .. })
    ));
    assert!(failure
        .publication
        .as_ref()
        .unwrap()
        .public_bindings()
        .is_empty());
    assert_eq!(failure.receipts.len(), 1);
    assert_eq!(failure.receipts[0].status, WorkbenchItemStatus::Committed);
    assert_eq!(failure.receipts[0].installed_bindings, ["privateProgress"]);
    assert!(matches!(
        failure.source,
        ResidentActorWorkbenchError::PrivatePublication { ref source, .. }
            if matches!(source.as_ref(), ResidentActorWorkbenchError::Resident(
                ResidentError::Session(SessionError::RecoveryStore(error))
            ) if error.kind == StoreErrorKind::RecoveryRequired)
    ));
    assert_eq!(
        session.public_visibility_snapshot_in(scope).unwrap(),
        before
    );
    assert_eq!(recovery.public_owner(&owner), confirmed);
    assert_eq!(
        control
            .native_cancel()
            .load(std::sync::atomic::Ordering::Acquire),
        cancel
    );
    let holes = session
        .parked_holes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    machines.settle_suspended(receipt, session, holes);
}

#[test]
fn generation_reset_response_projection_preserves_receipts_and_rejects_trailing_work() {
    // These are data-only response projections. Runtime owner/empty-frontier
    // qualification is exercised by the real superseded-private-execution tests.
    use tidepool_runtime::session::{ScopeRetirement, SupersededPrivateExecution};
    for trailing in [false, true] {
        let mut original = response(WorkbenchRunStatus::Committed);
        original.items[0].installed_bindings.clear();
        original.items[0].output = "retained committed reload receipt".into();
        if !trailing {
            original.total = original.next_index;
        }
        let original = workbench_response(
            if trailing {
                WorkbenchRunStatus::Completed
            } else {
                WorkbenchRunStatus::Committed
            },
            original.items,
            original.next_index,
            original.total,
            None,
        );
        let expected_items = original.items.clone();
        let result = mark_generation_reset(
            Ok(KernelStep::Continue(original)),
            SupersededPrivateExecution::Empty {
                retirement: ScopeRetirement::default(),
            },
        );
        let Ok(KernelStep::Continue(actual)) = result else {
            panic!("settled response stays terminal Ok")
        };
        assert_eq!(actual.items, expected_items);
        assert_eq!(actual.next_index, 1);
        if trailing {
            assert_eq!(actual.status, WorkbenchRunStatus::Rejected);
            assert!(matches!(
                actual.publication,
                Some(WorkbenchPublicationOutcome::Rejected { .. })
            ));
            assert_eq!(actual.total, 2);
            assert_eq!(actual.items[1].status, WorkbenchItemStatus::NotRun);
        } else {
            assert_eq!(actual.status, WorkbenchRunStatus::Committed);
            assert_eq!(
                actual.publication,
                Some(WorkbenchPublicationOutcome::NotPublished {
                    reason: WorkbenchNotPublishedReason::GenerationReset,
                })
            );
        }
    }
}

#[test]
fn discarded_generation_writes_remain_rejected_when_continuation_fails() {
    use tidepool_runtime::session::{
        DiscardedPrivateWrites, ScopeRetirement, SupersededCompilerProgress,
        SupersededPrivateExecution,
    };
    let failure = WorkbenchExecutionFailure {
        receipts: vec![receipt()],
        point: WorkbenchFailurePoint::InputUnit { index: 0 },
        publication: None,
        total: 1,
        source: ResidentActorWorkbenchError::ActorProtocol("original suffix failure".into()),
    };
    let result = mark_generation_reset(
        Err(failure),
        SupersededPrivateExecution::Discarded {
            writes: DiscardedPrivateWrites {
                bindings: Vec::new(),
                declarations: true,
                accepted_host_mounts: 0,
                compiler_progress: SupersededCompilerProgress::Checked,
            },
            retirement: ScopeRetirement::default(),
        },
    );
    let failure = result.unwrap_err();
    assert!(failure
        .source
        .to_string()
        .contains("original suffix failure"));
    assert_eq!(failure.receipts, [receipt()]);
    let Some(WorkbenchPublicationOutcome::Rejected { detail }) = failure.publication else {
        panic!("discarded declarations stay explicit even on failed suffix")
    };
    assert!(detail.contains("declarations: true"));
    assert!(detail.contains("reload committed"));
}

#[test]
fn active_unit_failure_keeps_receipts_and_typed_cause_through_publication_failure() {
    for cancelled in [false, true] {
        let mut cursor = WorkbenchCursor::default();
        cursor.receipts.push(receipt());
        cursor.index = 1;
        cursor.unit.recovered_bindings = vec!["retained".into(), "retained".into()];
        let operation = WorkbenchOperationReceipt {
            display_publication: None,
            display: None,
            id: WorkbenchOperationId {
                execution: WorkbenchExecutionId::from_digest([31; 16]),
                input_unit_index: 1,
                effect_ordinal: 0,
            },
            effect: "reloadSource".into(),
            disposition: WorkbenchOperationDisposition::Committed,
        };
        cursor.unit.operations.push(operation.clone());
        let source = if cancelled {
            ResidentActorWorkbenchError::InvocationCancelled
        } else {
            ResidentActorWorkbenchError::ActorProtocol("suffix failed".into())
        };
        let original = workbench_failure_from_active_unit(&mut cursor, 3, source);
        assert!(cursor.unit.operations.is_empty());
        assert_eq!(original.receipts.len(), 2);
        assert_eq!(original.receipts[0], receipt());
        assert_eq!(original.receipts[1].operations, [operation]);
        assert_eq!(original.receipts[1].installed_bindings, ["retained"]);
        let expected = original.receipts.clone();
        let failure = private_publication_failure(
            Err(original),
            ResidentActorWorkbenchError::ActorProtocol("cleanup refused".into()),
            WorkbenchPublicationOutcome::Rejected {
                detail: "cleanup refused".into(),
            },
        );
        assert_eq!(failure.receipts, expected);
        assert_eq!(failure.total, 3);
        assert!(matches!(
            failure.point,
            WorkbenchFailurePoint::InputUnit { index: 1 }
        ));
        assert_eq!(
            failure.source.execution_failure_kind(),
            cancelled.then_some(tidepool_runtime::session::PreparedFailureKind::Cancelled)
        );
        let ResidentActorWorkbenchError::FinalizationAfterFailure { primary, secondary } =
            failure.source
        else {
            panic!("both typed causes retained")
        };
        assert!(
            matches!(*secondary, ResidentActorWorkbenchError::ActorProtocol(ref text) if text == "cleanup refused")
        );
        if cancelled {
            assert!(matches!(
                *primary,
                ResidentActorWorkbenchError::InvocationCancelled
            ));
        } else {
            assert!(
                matches!(*primary, ResidentActorWorkbenchError::ActorProtocol(ref text) if text == "suffix failed")
            );
        }
    }
}

#[test]
fn cleanup_uncertainty_keeps_typed_cancellation_and_original_receipts() {
    let actor = ActorRef::first(crate::ActorId(71));
    let original = private_publication_failure(
        Err(WorkbenchExecutionFailure {
            receipts: vec![receipt()],
            point: WorkbenchFailurePoint::InputUnit { index: 0 },
            publication: None,
            total: 2,
            source: ResidentActorWorkbenchError::InvocationCancelled,
        }),
        ResidentActorWorkbenchError::ActorProtocol("publication refused".into()),
        WorkbenchPublicationOutcome::Rejected {
            detail: "publication refused".into(),
        },
    );
    let finalized = WorkbenchFinalizationResult {
        result: Err(workbench_invocation_failure(actor, original)),
        cleanup_confirmed: true,
    };
    let finalized = retain_cleanup_failure(actor, "native cleanup unconfirmed".into(), finalized);
    assert!(!finalized.cleanup_confirmed);
    let error = finalized.result.as_ref().unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(error.receipts(), [receipt()]);
    assert!(matches!(
        error,
        KernelInvocationFailure::CleanupUnconfirmed {
            source: Some(_),
            ..
        }
    ));
    let exit = crate::CellExit::from_reply(
        WorkbenchExecutionId::from_digest([72; 16]),
        &finalized.result,
        false,
        false,
    );
    assert_eq!(exit.cause, crate::CellExitCause::Cancelled);
    assert!(!exit.permits_context_commit());
}
