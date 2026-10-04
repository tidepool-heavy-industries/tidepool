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
        WorkbenchPublicationOutcome::DurabilityUnconfirmed {
            bindings: vec!["visible".into()],
            detail: "directory sync failed".into(),
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
