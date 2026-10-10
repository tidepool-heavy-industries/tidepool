use super::*;

fn receipt() -> WorkbenchItemReceipt {
    WorkbenchItemReceipt {
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

#[test]
fn prepared_publication_observing_a_sibling_fence_requires_recovery() {
    sibling_fence_before_preflight(false);
}

#[test]
fn prepared_publication_observing_a_sibling_fence_preserves_recovery_during_cancellation() {
    sibling_fence_before_preflight(true);
}

fn sibling_fence_before_preflight(cancel: bool) {
    use crate::recovery::recovery_test_fixture::RecoveryFixture;
    use tidepool_runtime::session::{
        fresh_session_id, ModuleEnv, PersistentSession, RecoveryDomainId, RecoveryPublicOwner,
        SessionError, SessionLib, StoreErrorKind,
    };

    let root = tempfile::tempdir().unwrap();
    let recovery = RecoveryFixture::create(root.path());
    let mut lib = SessionLib::open(
        fresh_session_id(),
        root.path().join("machine-a"),
        ModuleEnv::standalone_default(),
    )
    .unwrap();
    tidepool_testing::with_settlement(|settlement| recovery.attach_root(&mut lib, settlement));
    let mut session = PersistentSession::new(Some(lib), 1024);
    let scope = session.mint_isolated_scope();
    let owner = RecoveryPublicOwner::new(&crate::ActorPath::parse("root/a").unwrap(), 1).unwrap();
    assert!(matches!(
        session
            .initialize_durable_public_scope(owner.clone(), scope)
            .unwrap(),
        tidepool_runtime::session::PublicManifestCommit::Durable
    ));
    let confirmed = recovery.public_owner(&owner);
    let admitted = session
        .begin_durable_private_execution(&owner, scope)
        .unwrap();
    let prepared = session
        .freeze_execution_intent(&admitted, Vec::new(), Vec::new())
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
}
