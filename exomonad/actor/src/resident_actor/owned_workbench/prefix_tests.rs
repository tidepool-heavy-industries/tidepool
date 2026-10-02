use super::*;

fn receipt(status: WorkbenchItemStatus) -> WorkbenchItemReceipt {
    WorkbenchItemReceipt {
        index: 0,
        kind: None,
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
        summary: None,
        items,
        next_index: 1,
        total: 2,
    };
    let failure = |receipts| WorkbenchExecutionFailure {
        receipts,
        failed_index: 1,
        total: 2,
        source: ResidentActorWorkbenchError::ActorProtocol("native failure".into()),
    };
    let rejected = Ok(KernelStep::Continue(response(
        WorkbenchRunStatus::Rejected,
        vec![receipt(WorkbenchItemStatus::Committed)],
    )));
    let failed = Err(failure(vec![receipt(WorkbenchItemStatus::Committed)]));
    assert!(private_publication_required(&rejected));
    assert!(private_publication_required(&failed));
    assert!(!private_publication_required(&Ok(KernelStep::Continue(
        response(
            WorkbenchRunStatus::Rejected,
            vec![receipt(WorkbenchItemStatus::NotRun)]
        )
    ))));
    assert!(!private_publication_required(&Err(failure(Vec::new()))));
    assert!(!private_publication_required(&Ok(KernelStep::Continue(
        response(
            WorkbenchRunStatus::RequestCancelled,
            vec![receipt(WorkbenchItemStatus::Committed)]
        )
    ))));

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
