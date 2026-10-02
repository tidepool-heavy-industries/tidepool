use super::*;
use tidepool_runtime::session::ResidentError;

fn receipt(status: WorkbenchItemStatus) -> WorkbenchItemReceipt {
    WorkbenchItemReceipt {
        index: 0,
        kind: Some(WorkbenchCellItemKind::Statement),
        span: None,
        source_items: Vec::new(),
        status,
        output: String::new(),
        diagnostics: Vec::new(),
        failure_layer: None,
        warnings: Vec::new(),
        installed_bindings: vec!["completedPrefix".into()],
        operations: Vec::new(),
        terminal_transfer: None,
    }
}

#[test]
fn prefix_publication_preserves_failed_cell_eligibility_and_cancellation_veto() {
    let response = |status, items| WorkbenchResponse {
        status,
        publication: None,
        summary: None,
        items,
        next_index: 1,
        total: 2,
    };
    let failure = |receipts| WorkbenchExecutionFailure {
        receipts,
        point: WorkbenchFailurePoint::InputUnit { index: 1 },
        publication: None,
        total: 2,
        source: ResidentActorWorkbenchError::ActorProtocol("native failure".into()),
    };
    let rejected = Ok(KernelStep::Continue(response(
        WorkbenchRunStatus::Rejected,
        vec![receipt(WorkbenchItemStatus::Committed)],
    )));
    let failed = Err(failure(vec![receipt(WorkbenchItemStatus::Committed)]));
    assert_eq!(
        private_publication_intent(&rejected),
        Some(ExecutionPublicationIntent::CommittedNativePrefix)
    );
    assert_eq!(
        private_publication_intent(&failed),
        Some(ExecutionPublicationIntent::CommittedNativePrefix)
    );
    assert!(
        private_publication_intent(&Ok(KernelStep::Continue(response(
            WorkbenchRunStatus::Rejected,
            vec![receipt(WorkbenchItemStatus::NotRun)]
        ))))
        .is_none()
    );
    assert!(private_publication_intent(&Err(failure(Vec::new()))).is_none());
    assert!(
        private_publication_intent(&Ok(KernelStep::Continue(response(
            WorkbenchRunStatus::RequestCancelled,
            vec![receipt(WorkbenchItemStatus::Committed)]
        ))))
        .is_none()
    );

    let mut declaration = receipt(WorkbenchItemStatus::Committed);
    declaration.kind = Some(WorkbenchCellItemKind::Declaration);
    declaration.installed_bindings = vec!["privateDeclaration".into()];
    let declarations_only = Err(failure(vec![declaration.clone()]));
    assert!(private_publication_intent(&declarations_only).is_none());
    let mixed = Err(failure(vec![
        declaration,
        receipt(WorkbenchItemStatus::Committed),
    ]));
    assert_eq!(
        private_publication_bindings(&mixed),
        vec!["completedPrefix"]
    );
    assert_eq!(
        private_publication_intent(&Ok(KernelStep::Continue(response(
            WorkbenchRunStatus::Committed,
            vec![]
        )))),
        Some(ExecutionPublicationIntent::CompletedCell)
    );

    let decision = tidepool_runtime::session::PublicationDecision::new();
    decision.claim_commit().unwrap().published();
    let actor = crate::ActorRef::first(crate::ActorId(1));
    for (result, cause) in [
        (
            rejected.map_err(|_| unreachable!()),
            crate::CellExitCause::Rejected,
        ),
        (
            Err(KernelInvocationFailure::Failed {
                actor,
                detail: "native failure".into(),
            }),
            crate::CellExitCause::Failed,
        ),
    ] {
        let exit = crate::CellExit::from_reply(
            WorkbenchExecutionId::from_digest([24; 16]),
            &result,
            true,
            false,
        );
        assert_eq!(exit.cause, cause);
        assert!(!exit.permits_context_commit());
    }
    let cancelled = tidepool_runtime::session::PublicationDecision::new();
    assert_eq!(
        cancelled.request_cancellation(),
        tidepool_runtime::session::PublicationCancellation::RequestedBeforeCommit
    );
    assert!(cancelled.claim_commit().is_none());
}

#[test]
fn prefix_publication_failure_retains_original_run_diagnostic_and_secondary_cause() {
    let original = ResidentActorWorkbenchError::Delivered(ResidentError::Run(
        tidepool_runtime::RuntimeError::Jit(tidepool_effect::error::EffectError::Handler(
            "native failure after delivered response".into(),
        )),
    ));
    let diagnostic = original.failure_diagnostic().unwrap();
    let failure = WorkbenchExecutionFailure {
        receipts: vec![receipt(WorkbenchItemStatus::Committed)],
        point: WorkbenchFailurePoint::InputUnit { index: 1 },
        publication: None,
        total: 3,
        source: original,
    };
    let failed = private_publication_failure(
        Err(failure),
        ResidentActorWorkbenchError::ActorProtocol("manifest rename failed".into()),
        WorkbenchPublicationOutcome::Rejected {
            detail: "manifest rename failed".into(),
        },
    );
    let retained = failed.source.failure_diagnostic().unwrap();
    assert_eq!(retained.phase, tidepool_toolchain::failclass::Phase::Run);
    assert_eq!(retained.class, diagnostic.class);
    assert_eq!(retained.message, diagnostic.message);
    assert!(matches!(
        failed.source.primary_failure(),
        ResidentActorWorkbenchError::Delivered(_)
    ));
    assert_eq!(
        resident_actor_failure_layer(&failed.source),
        Some(WorkbenchFailureLayer::Effect)
    );
    assert!(failed
        .source
        .to_string()
        .contains("native failure after delivered response"));
    assert!(failed.source.to_string().contains("manifest rename failed"));
    assert!(
        matches!(failed.source, ResidentActorWorkbenchError::PrefixPublication { publication, .. }
        if matches!(*publication, ResidentActorWorkbenchError::ActorProtocol(_)))
    );
    assert_eq!(failed.receipts.len(), 1);
    assert_eq!(failed.point, WorkbenchFailurePoint::InputUnit { index: 1 });
    assert_eq!(
        failed.publication,
        Some(WorkbenchPublicationOutcome::Rejected {
            detail: "manifest rename failed".into()
        })
    );
    assert_eq!(failed.total, 3);
}
