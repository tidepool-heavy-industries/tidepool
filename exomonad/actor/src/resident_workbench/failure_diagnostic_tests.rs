use super::*;

#[test]
fn kernel_failure_roundtrip_keeps_compiler_diagnostic_and_plain_failure() {
    let compiler = ResidentActorWorkbenchError::Compile(CompileError::MissingOutput(
        PathBuf::from("captured-release.cbor"),
    ))
    .into_kernel_behavior_error();
    for original in [compiler, crate::KernelBehaviorError::new("scope released")] {
        let expected = original.clone();
        let retained = ResidentActorWorkbenchError::from(original);
        assert_eq!(retained.failure_diagnostic(), expected.diagnostic);
        assert_eq!(retained.into_kernel_behavior_error(), expected);
    }
}

#[test]
fn activation_committed_failure_preserves_compiler_infrastructure_diagnostic() {
    let binding = tidepool_repr::SessionVarId::from_extract(41);
    let error = ResidentActorWorkbenchError::ActivationBindingCommitted {
        binding,
        source: Box::new(ResidentActorWorkbenchError::InputCompilation {
            stage: ActivationCompileStage::Preview,
            error: CompileError::MissingOutput(PathBuf::from("preview-receipt.cbor")),
        }),
    };
    let diagnostic = error
        .failure_diagnostic()
        .expect("committed diagnostic retained");
    assert_eq!(diagnostic.class, FailureClass::Infra);
    assert_eq!(
        diagnostic.cause,
        Some(tidepool_toolchain::failclass::CompileFailureCause::MissingOutput),
    );
    let ResidentActorWorkbenchError::ActivationBindingCommitted {
        binding: retained,
        source,
    } = error
    else {
        panic!("committed preview failure lost its disposition")
    };
    assert_eq!(retained, binding);
    assert!(source.to_string().contains("preview-receipt.cbor"));
}

#[test]
fn activation_suspension_keeps_primary_and_abort_failure() {
    let error = ResidentActorWorkbenchError::ActivationBindingCommitted {
        binding: tidepool_repr::SessionVarId::from_extract(42),
        source: Box::new(ResidentActorWorkbenchError::ActivationPreviewSuspended {
            cleanup: Some(ResidentError::InvalidActivationInput { site: 43 }),
        }),
    };
    let rendered = error.to_string();
    assert!(rendered.contains("pure activation preview suspended"));
    assert!(rendered.contains("InvalidActivationInput"));
    assert!(rendered.contains("43"));
}

#[test]
fn activation_compile_failure_keeps_stage_typed_diagnostics_and_bounded_text() {
    use tidepool_toolchain::diag::{DiagSpan, DiagnosticSeverity, ExtractDiag};
    for stage in [
        ActivationCompileStage::Interface,
        ActivationCompileStage::Preview,
    ] {
        let message = format!(
            "original compiler detail\n{}\nlast compiler detail",
            "λ".repeat(8192)
        );
        let error = ResidentActorWorkbenchError::InputCompilation {
            stage,
            error: CompileError::Diagnostics(vec![ExtractDiag {
                span: Some(DiagSpan {
                    file: "CellCheck.hs".into(),
                    start_line: 46,
                    start_col: 3,
                    end_line: 46,
                    end_col: 19,
                }),
                severity: DiagnosticSeverity::Error,
                message: message.clone(),
            }]),
        };
        let diagnostic = error.failure_diagnostic().unwrap();
        assert_eq!(diagnostic.class, FailureClass::UserHaskell);
        assert_eq!(
            diagnostic.cause,
            Some(tidepool_toolchain::failclass::CompileFailureCause::SourceDiagnostics,)
        );
        assert!(diagnostic.message.len() <= ACTIVATION_COMPILE_DIAGNOSTIC_BYTES);
        assert!(diagnostic.message.contains("CellCheck.hs:46:3"));
        assert!(diagnostic.message.contains("original compiler detail"));
        assert!(diagnostic.message.contains("last compiler detail"));
        assert!(error.to_string().contains(&stage.to_string()));
        assert!(error.to_string().contains("original compiler detail"));
        let ResidentActorWorkbenchError::InputCompilation {
            stage: retained_stage,
            error: CompileError::Diagnostics(retained),
        } = error
        else {
            panic!("typed compiler failure lost")
        };
        assert_eq!(retained_stage, stage);
        assert_eq!(retained[0].message, message);
    }
}

#[test]
fn parser_failure_routes_source_only_and_preserves_non_source_classes() {
    use tidepool_toolchain::diag::{DiagnosticSeverity, ExtractDiag};
    let diagnostic = ExtractDiag {
        span: None,
        severity: DiagnosticSeverity::Error,
        message: "parse error λ".into(),
    };
    let source = cell_check_error(
        CompileError::Diagnostics(vec![diagnostic.clone()]).into(),
        ":}",
    );
    let ResidentActorWorkbenchError::CellCheck(source) = source else {
        panic!("authored parse failure must use source-aware rejection")
    };
    assert!(
        source.items.is_none(),
        "a parser failure has no accepted source plan"
    );
    let CompileError::Diagnostics(retained) = source.error else {
        panic!("source diagnostic lost")
    };
    assert_eq!(retained[0].message, diagnostic.message);
    for error in [
        CompileError::MissingOutput(PathBuf::from("missing-output")),
        CompileError::Io(std::io::Error::other("controlled IO failure")),
        CompileError::WorkerFailure(vec![diagnostic.clone()]),
        CompileError::InputRejected(vec![diagnostic]),
        CompileError::ExtractFailed("controlled contract failure".into()),
        CompileError::MalformedDiagnostics("controlled wire failure".into()),
    ] {
        let expected = classify_compile(&error);
        let mapped = cell_check_error(error.into(), ":}");
        assert!(matches!(
            mapped,
            ResidentActorWorkbenchError::CompileInfrastructure(_)
        ));
        let actual = mapped.failure_diagnostic().expect("classified failure");
        assert_eq!(actual.class, expected.class);
        assert_eq!(actual.phase, expected.phase);
        assert_eq!(actual.cause, expected.cause);
    }
}

#[test]
fn preflight_infrastructure_failure_keeps_compiler_class_and_cause() {
    let failure = CompileError::MissingOutput(std::path::PathBuf::from("missing-output")).into();
    let error = cell_check_error(failure, "authored cell");
    let diagnostic = error
        .failure_diagnostic()
        .expect("compile failure classified");
    assert_eq!(diagnostic.class, FailureClass::Infra);
    assert_eq!(
        diagnostic.phase,
        tidepool_toolchain::failclass::Phase::Compile
    );
    assert_eq!(
        diagnostic.cause,
        Some(tidepool_toolchain::failclass::CompileFailureCause::MissingOutput)
    );
    assert!(error.to_string().contains("missing-output"));
    assert!(error
        .to_string()
        .starts_with("resident workbench compiler infrastructure failed:"));
}

#[test]
fn renderer_production_keeps_action_and_unconfirmed_close_together() {
    use tidepool_runtime::session::turn::ActivationRendererFailure;
    use tidepool_runtime::session::RendererProductionFailure;

    let close = tidepool_runtime::CompilerTransactionClose::Unconfirmed(
        tidepool_extract_cmd::CompilerTransactionCloseEvidence::new(
            tidepool_extract_cmd::CompilerTransactionCloseReason::FailedRequest,
            tidepool_extract_cmd::CompilerTransactionRetirement::DaemonUnobserved {
                disconnect: None,
            },
        ),
    );
    let error = ResidentActorWorkbenchError::RendererProduction {
        failure: Arc::new(RendererProductionFailure::CloseUnconfirmed {
            close: close.clone(),
            action_error: Some(ActivationRendererFailure::from(
                CompileError::ExtractFailed("primary renderer action".into()),
            )),
        }),
        producer_join: None,
    };
    let ResidentActorWorkbenchError::RendererProduction { failure, .. } = &error else {
        panic!("renderer failure")
    };
    let RendererProductionFailure::CloseUnconfirmed {
        close: retained_close,
        action_error: Some(ActivationRendererFailure::Frontend(primary)),
    } = failure.as_ref()
    else {
        panic!("compound renderer failure lost its typed action and close facts")
    };
    assert_eq!(retained_close, &close);
    assert!(matches!(
        &primary.error,
        CompileError::ExtractFailed(detail) if detail == "primary renderer action"
    ));
    let diagnostic = error
        .failure_diagnostic()
        .expect("compound renderer failure retains infrastructure evidence");
    assert_eq!(diagnostic.class, FailureClass::Infra);
    assert_eq!(
        diagnostic.cause,
        Some(tidepool_toolchain::failclass::CompileFailureCause::CompilerCloseUnconfirmed)
    );
    assert!(diagnostic.message.contains("primary renderer action"));
    assert!(diagnostic.message.contains("compiler close is unconfirmed"));
    assert!(error.to_string().contains("primary renderer action"));
    assert!(!error.is_tool_presentation_failure());
    assert!(error.requires_recovery());
}

#[test]
fn renderer_production_keeps_action_and_publication_refusal_together() {
    use tidepool_runtime::session::turn::ActivationRendererFailure;
    use tidepool_runtime::session::RendererProductionFailure;

    let error = ResidentActorWorkbenchError::RendererProduction {
        failure: Arc::new(RendererProductionFailure::Publication {
            error: tidepool_runtime::CompilerTransactionPublicationError::DifferentOwner,
            close: tidepool_runtime::CompilerTransactionClose::NotStarted,
            action_error: Some(ActivationRendererFailure::from(
                CompileError::ExtractFailed("primary renderer action".into()),
            )),
        }),
        producer_join: None,
    };
    let diagnostic = error
        .failure_diagnostic()
        .expect("compound renderer failure retains infrastructure evidence");
    assert_eq!(diagnostic.class, FailureClass::Infra);
    assert!(diagnostic.message.contains("primary renderer action"));
    assert!(diagnostic.message.contains("compiler publication failed"));
    assert!(error.to_string().contains("primary renderer action"));
    assert!(!error.is_tool_presentation_failure());
    assert!(error.requires_recovery());
}

#[test]
fn renderer_clean_admission_failure_preserves_the_resident_recovery_requirement() {
    use tidepool_runtime::session::turn::ActivationRendererFailure;
    use tidepool_runtime::session::{
        RendererProductionFailure, SessionError, StoreError, StoreErrorKind,
    };
    for kind in [StoreErrorKind::RecoveryRequired, StoreErrorKind::Storage] {
        let cause = || {
            ResidentError::Session(SessionError::RecoveryStore(Box::new(StoreError {
                path: "renderer-recovery".into(),
                kind,
                detail: "typed resident failure".into(),
            })))
        };
        let original = ResidentActorWorkbenchError::Resident(cause());
        let renderer = ResidentActorWorkbenchError::RendererProduction {
            failure: Arc::new(RendererProductionFailure::Compile(
                ActivationRendererFailure::Admission(cause()),
            )),
            producer_join: None,
        };
        assert_eq!(renderer.requires_recovery(), original.requires_recovery());
        assert_eq!(
            renderer.requires_recovery(),
            kind == StoreErrorKind::RecoveryRequired
        );
        assert_eq!(renderer.failure_diagnostic(), original.failure_diagnostic());
    }
}

#[test]
fn renderer_language_failure_keeps_primary_when_original_source_is_fenced() {
    use tidepool_toolchain::diag::{DiagnosticSeverity, ExtractDiag};
    let mut session = tidepool_runtime::session::PersistentSession::new(None, 1024);
    let original = session.mint_notebook_scope_with_lease(None).unwrap();
    let target = session.mint_notebook_scope_with_lease(None).unwrap();
    let _fence = original.source_admission().fence().unwrap();
    let refusal = original
        .with_open_source_admission(|| panic!("fenced source admitted"))
        .unwrap_err();
    assert!(target.admit_source().is_ok());
    let primary = Arc::new(
        tidepool_runtime::session::RendererProductionFailure::Compile(
            tidepool_runtime::session::turn::ActivationRendererFailure::from(
                CompileError::Diagnostics(vec![ExtractDiag {
                    span: None,
                    severity: DiagnosticSeverity::Error,
                    message: "retained renderer source diagnostic".into(),
                }]),
            ),
        ),
    );
    let error = ResidentActorWorkbenchError::RendererAdmission {
        failure: primary.clone(),
        producer_join: None,
        admission: Box::new(ResidentActorWorkbenchError::SourceAdmission(refusal)),
    };
    let diagnostic = error.failure_diagnostic().unwrap();
    assert_eq!(diagnostic.class, FailureClass::UserHaskell);
    assert!(diagnostic
        .message
        .contains("retained renderer source diagnostic"));
    assert!(diagnostic.message.contains("Fenced"));
    assert!(!error.is_tool_presentation_failure());
    assert!(!error.is_observation_budget_exhausted());
    assert!(!error.requires_recovery());
    let ResidentActorWorkbenchError::RendererAdmission {
        failure, admission, ..
    } = error
    else {
        panic!("both causes must remain typed");
    };
    assert!(Arc::ptr_eq(&failure, &primary));
    assert!(matches!(
        *admission,
        ResidentActorWorkbenchError::SourceAdmission(
            tidepool_runtime::session::SourceAdmissionError::Closed { .. }
        )
    ));
}

#[test]
fn renderer_admission_failure_keeps_independent_recovery_requirement() {
    use tidepool_runtime::session::{SessionError, StoreError, StoreErrorKind};
    for kind in [StoreErrorKind::RecoveryRequired, StoreErrorKind::Storage] {
        let error = ResidentActorWorkbenchError::RendererAdmission {
            failure: Arc::new(
                tidepool_runtime::session::RendererProductionFailure::Compile(
                    tidepool_runtime::session::turn::ActivationRendererFailure::from(
                        CompileError::MissingOutput("retained-renderer-output".into()),
                    ),
                ),
            ),
            producer_join: None,
            admission: Box::new(ResidentActorWorkbenchError::Resident(
                ResidentError::Session(SessionError::RecoveryStore(Box::new(StoreError {
                    path: "fresh-admission-store".into(),
                    kind,
                    detail: "independent admission obligation".into(),
                }))),
            )),
        };
        assert_eq!(
            error.requires_recovery(),
            kind == StoreErrorKind::RecoveryRequired
        );
        let diagnostic = error.failure_diagnostic().unwrap();
        assert_eq!(
            diagnostic.cause,
            Some(tidepool_toolchain::failclass::CompileFailureCause::MissingOutput)
        );
        assert!(diagnostic.message.contains("retained-renderer-output"));
        assert!(diagnostic
            .message
            .contains("independent admission obligation"));
    }
}
