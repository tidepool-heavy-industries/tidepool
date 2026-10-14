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
