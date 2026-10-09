//! Bounded presentation preserves typed publication and effect outcomes.

use super::*;
use proptest::prelude::*;
use tidepool_runtime::session::{
    WorkbenchExecutionId, WorkbenchFailurePoint, WorkbenchItemReceipt, WorkbenchItemStatus,
    WorkbenchOperationDisposition as Disposition, WorkbenchOperationId, WorkbenchOperationReceipt,
    WorkbenchPublicationOutcome as Publication,
};

fn origin() -> OperationId {
    OperationId {
        origin: ConversationIdentity::Embedded {
            run: "projection-run".into(),
            actor: AgentPath("/root".into()),
            incarnation: "7".into(),
        },
        request: harness::model::RequestId("original-request".into()),
        call: harness::model::CallId("original-call".into()),
    }
}

fn receipt(index: usize, output: String, dispositions: &[Disposition]) -> WorkbenchItemReceipt {
    WorkbenchItemReceipt {
        index,
        kind: None,
        span: None,
        source_items: Vec::new(),
        status: WorkbenchItemStatus::Stopped,
        output,
        value: Some(json!({"Left": "application refused after a completed effect"})),
        diagnostics: Vec::new(),
        failure_layer: None,
        warnings: Vec::new(),
        installed_bindings: Vec::new(),
        operations: dispositions
            .iter()
            .enumerate()
            .map(|(ordinal, disposition)| WorkbenchOperationReceipt {
                id: WorkbenchOperationId {
                    execution: WorkbenchExecutionId::from_digest([7; 16]),
                    input_unit_index: index,
                    effect_ordinal: ordinal,
                },
                effect: "record_send".into(),
                disposition: *disposition,
                display: None,
                display_publication: None,
            })
            .collect(),
        terminal_transfer: None,
    }
}

fn error(
    receipts: Vec<WorkbenchItemReceipt>,
    detail: String,
    publication: Publication,
) -> ResidentToolError {
    ResidentToolError::Invocation(exomonad_actor::KernelInvocationFailure::Workbench(
        exomonad_actor::KernelWorkbenchFailure {
            actor: ActorRef::first(exomonad_actor::ActorId(7)),
            point: WorkbenchFailurePoint::InputUnit {
                index: receipts.len(),
            },
            total: receipts.len() + 1,
            receipts,
            publication: Some(publication),
            detail,
            diagnostic: Some(tidepool_toolchain::failclass::classify_compile(
                &tidepool_toolchain::CompileError::ExtractFailed("retained owner missing".into()),
            )),
        },
    ))
}

fn check_failure(failure: &ToolFailure, operation: &OperationId) {
    assert!(failure.message().len() <= TOOL_ERROR_MESSAGE_BYTE_BUDGET);
    let metadata = failure.metadata().expect("bounded recovery metadata");
    assert!(serde_json::to_vec(metadata).unwrap().len() <= 8192);
    assert_eq!(metadata["originalOperation"], json!(operation));
    assert_eq!(
        metadata["retainedEvidence"]["inspection"],
        FAILURE_INSPECTION
    );
    assert_eq!(metadata["retainedEvidence"]["modelTool"], false);
}

fn config() -> proptest::test_runner::Config {
    let mut config = proptest::test_runner::Config::default();
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(
            proptest::test_runner::FileFailurePersistence::Direct(path),
        ));
    }
    config
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn unicode_and_receipt_size_never_expand_the_model_error(
        characters in prop::collection::vec(any::<char>(), 0..4096),
        repetition in 1usize..5,
        output_repetition in 1usize..257,
        disposition in prop_oneof![
            Just(Disposition::Prepared), Just(Disposition::Read), Just(Disposition::Staged),
            Just(Disposition::Committed), Just(Disposition::Rejected), Just(Disposition::Unknown),
        ],
        depth in 0usize..20,
    ) {
        let detail: String = characters.into_iter().collect();
        let mut item = receipt(0, "receipt payload must not be formatted into the message".repeat(output_repetition), &[disposition]);
        let mut value = json!({"Left": "application failed"});
        for _ in 0..depth { value = json!({"diagnostic": value}); }
        item.value = Some(value);
        let operation = origin();
        let failure = provider_tool_error(error(vec![item], detail.repeat(repetition), Publication::NotPublished {
            reason: tidepool_runtime::session::WorkbenchNotPublishedReason::Failed,
        }), Some(&operation)).into_tool_failure();
        check_failure(&failure, &operation);
        prop_assert_eq!(&failure.metadata().unwrap()["items"][0]["operations"][0]["disposition"], &json!(disposition));
        prop_assert_eq!(&failure.metadata().unwrap()["publication"]["status"], &json!("notPublished"));
    }
}

#[test]
fn many_operations_and_diagnostics_keep_recovery_and_distinct_effect_outcomes() {
    let dispositions = [
        Disposition::Prepared,
        Disposition::Read,
        Disposition::Staged,
        Disposition::Committed,
        Disposition::Rejected,
        Disposition::Unknown,
    ]
    .repeat(32);
    let receipts = (0..32)
        .map(|index| receipt(index, "large retained output".repeat(2048), &dispositions))
        .collect();
    let operation = origin();
    let failure = provider_tool_error(
        error(
            receipts,
            "λ".repeat(8192),
            Publication::DurabilityUnconfirmed {
                bindings: vec!["large binding name".repeat(100); 100],
                detail: "publication detail".repeat(1000),
            },
        ),
        Some(&operation),
    )
    .into_tool_failure();
    check_failure(&failure, &operation);
    let metadata = failure.metadata().unwrap();
    assert_eq!(metadata["publication"]["status"], "durabilityUnconfirmed");
    assert_eq!(metadata["publication"]["bindingsOmitted"], 100);
    assert_eq!(metadata["itemsOmitted"], 32);
    assert_eq!(metadata["diagnosticOmitted"], true);
    assert_eq!(metadata["class"], "version-skew");
    assert_eq!(metadata["phase"], "compile");
    assert_eq!(metadata["applicationValuesOmitted"], true);
    for disposition in [
        "prepared",
        "read",
        "staged",
        "committed",
        "rejected",
        "unknown",
    ] {
        assert_eq!(metadata["operationOutcomes"][disposition], 1024);
    }
    assert!(failure.message().contains("durability is unconfirmed"));
    assert!(failure.message().contains(
        "native API reference does not establish admission, application success or release"
    ));
}

#[test]
fn committed_effect_does_not_erase_an_application_left_or_failure() {
    let operation = origin();
    let failure = provider_tool_error(
        error(
            vec![receipt(
                0,
                "Left application-failed".into(),
                &[Disposition::Committed],
            )],
            "later notebook unit failed".into(),
            Publication::Published {
                bindings: vec!["earlierBinding".into()],
            },
        ),
        Some(&operation),
    )
    .into_tool_failure();
    check_failure(&failure, &operation);
    let metadata = failure.metadata().unwrap();
    assert_eq!(metadata["publication"]["status"], "published");
    assert_eq!(
        metadata["items"][0]["operations"][0]["disposition"],
        "committed"
    );
    assert_eq!(
        metadata["items"][0]["value"]["Left"],
        "application refused after a completed effect"
    );
    assert!(failure.message().contains("later notebook unit failed"));
    assert!(failure.output_value().get("error").is_some());
}

#[test]
fn deeply_nested_receipt_value_is_reduced_without_losing_effect_identity() {
    let mut item = receipt(0, "prefix effect completed".into(), &[Disposition::Unknown]);
    let mut value = json!("diagnostic leaf");
    for _ in 0..32 {
        value = json!({"diagnostic": value});
    }
    item.value = Some(value);
    let operation = origin();
    let failure = provider_tool_error(
        error(
            vec![item],
            "observation failed".into(),
            Publication::Rejected {
                detail: "public bindings rejected".into(),
            },
        ),
        Some(&operation),
    )
    .into_tool_failure();
    check_failure(&failure, &operation);
    let metadata = failure.metadata().unwrap();
    assert_eq!(metadata["itemsReduced"], true);
    assert_eq!(
        metadata["items"][0]["operations"][0]["id"]["effectOrdinal"],
        0
    );
    assert_eq!(
        metadata["items"][0]["operations"][0]["disposition"],
        "unknown"
    );
    assert_eq!(metadata["class"], "version-skew");
    assert_eq!(metadata["publication"]["status"], "rejected");
}

#[test]
fn oversized_original_identity_is_explicitly_omitted_with_the_native_inspection_reference() {
    let mut operation = origin();
    operation.request.0 = "λ".repeat(8192);
    let failure = provider_tool_error(
        error(
            Vec::new(),
            "admission failed".into(),
            Publication::NotPublished {
                reason: tidepool_runtime::session::WorkbenchNotPublishedReason::Rejected,
            },
        ),
        Some(&operation),
    )
    .into_tool_failure();
    assert!(failure.message().len() <= TOOL_ERROR_MESSAGE_BYTE_BUDGET);
    assert!(failure.message().contains("admission failed"));
    let metadata = failure.metadata().expect("recovery fields must still fit");
    assert_eq!(metadata["originalOperationOmitted"], true);
    assert!(metadata.get("originalOperation").is_none());
    assert_eq!(
        metadata["retainedEvidence"]["inspection"],
        FAILURE_INSPECTION
    );
    assert_eq!(metadata["publication"]["reason"], "rejected");
}

#[test]
fn unconfirmed_cancellation_keeps_its_kind_and_bounds_retained_error_output() {
    let operation = origin();
    let output = native_terminal_output(
        Ok(json!({"application": "already returned"})),
        Err(error(
            vec![receipt(
                0,
                "retained prefix payload".repeat(2048),
                &[Disposition::Committed],
            )],
            "native cleanup remains unconfirmed".into(),
            Publication::Published {
                bindings: vec!["completedBinding".into()],
            },
        )),
        Some(&operation),
    );
    let JobOutput::CancellationUnconfirmed(message) = output else {
        panic!("projection must preserve unconfirmed cancellation");
    };
    assert!(message.len() <= TOOL_ERROR_MESSAGE_BYTE_BUDGET);
    assert!(message.contains("native cleanup remains unconfirmed"));
    assert!(message.contains("original-call"));
    assert!(message.contains("bindings were published"));
    assert!(!message.contains("retained prefix payload"));
}
