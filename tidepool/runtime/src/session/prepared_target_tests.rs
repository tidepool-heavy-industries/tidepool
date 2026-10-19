//! Prepared-target ownership controls through the real scoped compiler entry.

use super as session;
use crate::{
    finish_runtime_targets, prepare_runtime_targets_in_context, CompileError,
    CompilerTransactionCancellation, CompilerTransactionClose, CompilerTransactionPublicationError,
    RuntimeError, RuntimePreparedTargetsDraft, DEFAULT_NURSERY_SIZE,
};
use std::sync::Arc;
use tidepool_toolchain::artifacts;

fn prepared_target_source() -> (
    tempfile::TempDir,
    session::PersistentSession,
    Arc<session::RuntimeLexicalScopeLease>,
    Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
) {
    let root = tempfile::tempdir().unwrap();
    let recovery = session::recovery_test_fixture::RecoveryFixture::create(root.path());
    let mut lib = session::SessionLib::open(
        tidepool_repr::SessionId(9927),
        root.path(),
        session::ModuleEnv::standalone_default(),
    )
    .unwrap()
    .with_validation_include(vec![tidepool_testing::eval_harness::prelude_path()]);
    tidepool_testing::with_settlement(|report| recovery.attach_root(&mut lib, report)).unwrap();
    let mut state = session::PersistentSession::new(Some(lib), DEFAULT_NURSERY_SIZE);
    let scope = state
        .mint_detached_scope(tidepool_codegen::scope::ScopeId::ROOT)
        .unwrap();
    let source = state.adopt_initial_notebook_scope(scope).unwrap();
    let seeded = state
        .with_source_compiler_transaction(
            source.clone(),
            artifacts::CompileWorkload::Foreground,
            CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |state, report| {
                state.define_scoped_in(
                    scope,
                    &["preparedContextSeed :: Int", "preparedContextSeed = 0"],
                    report,
                )
            },
        )
        .unwrap();
    assert!(matches!(seeded.close(), CompilerTransactionClose::Clean));
    seeded.into_outcome().action.unwrap();
    let context = state.lib().current_exact_context_in(scope).unwrap();
    assert_ne!(context.toolchain_identity_sha256(), [0; 32]);
    (root, state, source, context)
}

fn prepare_target_control(
    owner: &session::RuntimeSourceCompilerTransaction,
    context: Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext>,
    expression: &str,
    report: &mut dyn FnMut(CompilerTransactionClose),
) -> Result<RuntimePreparedTargetsDraft, RuntimeError> {
    let source = session::assemble_expression_module(
        "{-# LANGUAGE DataKinds #-}\nmodule PreparedTargetsControl where\nimport Control.Monad.Freer (Eff)\n",
        "result", "'[]", expression, session::ExpressionLift::Effectful,
    );
    let include = [tidepool_testing::eval_harness::prelude_path()];
    prepare_runtime_targets_in_context(
        owner,
        &artifacts::CompileInvocation {
            source: &source,
            targets: &[session::PREPARED_SCAFFOLD_TARGET],
            include: &include,
            fallback_module_name: "PreparedTargetsControl",
        },
        context,
        |_, _, _| {},
        report,
    )
}

const PREPARED_TARGET_CONTROL: &str = "pure (42 :: Int)";

#[test]
fn prepared_targets_reject_foreign_settlement_and_retired_original() {
    tidepool_testing::eval_harness::require_extract();
    let (_root, _state, source, context) = prepared_target_source();
    let first = source
        .with_source_compiler_transaction(
            artifacts::CompileWorkload::Foreground,
            CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, report| {
                prepare_target_control(owner, context.clone(), PREPARED_TARGET_CONTROL, report)
            },
        )
        .unwrap();
    assert!(matches!(
        first.close(),
        CompilerTransactionClose::Clean | CompilerTransactionClose::NotStarted
    ));
    let draft = first.into_outcome().action.unwrap();
    let foreign = source
        .with_source_compiler_transaction(
            artifacts::CompileWorkload::Foreground,
            CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |_, _| Ok(draft),
        )
        .unwrap();
    assert!(matches!(
        finish_runtime_targets(foreign),
        Err(RuntimeError::Compile(CompileError::CompilerPublication(
            CompilerTransactionPublicationError::DifferentOwner
        )))
    ));

    let actual = source
        .with_source_compiler_transaction(
            artifacts::CompileWorkload::Foreground,
            CompilerTransactionCancellation::new(),
            None,
            |_| {},
            |owner, report| {
                prepare_target_control(owner, context.clone(), PREPARED_TARGET_CONTROL, report)
            },
        )
        .unwrap();
    let prepared = finish_runtime_targets(actual).unwrap();
    assert_eq!(
        prepared
            .run(
                session::PREPARED_SCAFFOLD_TARGET,
                DEFAULT_NURSERY_SIZE,
                &mut frunk::HNil,
                &(),
                |_| {}
            )
            .unwrap()
            .to_json(),
        serde_json::json!(42)
    );
    source
        .source_admission()
        .fence()
        .unwrap()
        .retire(session::NotebookGenerationId::INITIAL)
        .unwrap();
    let mut entered = false;
    let refused = prepared.run(
        session::PREPARED_SCAFFOLD_TARGET,
        DEFAULT_NURSERY_SIZE,
        &mut frunk::HNil,
        &(),
        |_| entered = true,
    );
    assert!(matches!(
        refused,
        Err(RuntimeError::SourceAdmission(
            session::SourceAdmissionError::Closed { .. }
        ))
    ));
    assert!(
        !entered,
        "retired original cannot reach fresh engine execution"
    );
}

#[test]
fn prepared_targets_preserve_primary_error_with_uncertain_close() {
    tidepool_testing::eval_harness::require_extract();
    for action_fails in [false, true] {
        let (_root, _state, source, context) = prepared_target_source();
        let settled = source
            .with_source_compiler_transaction(
                artifacts::CompileWorkload::Foreground,
                CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, report| {
                    let source = if action_fails {
                        "missingTargetName"
                    } else {
                        PREPARED_TARGET_CONTROL
                    };
                    let result = prepare_target_control(owner, context.clone(), source, report);
                    assert_eq!(result.is_err(), action_fails);
                    // Aggregate injection is not a physical GHC END fault.
                    report(CompilerTransactionClose::Unconfirmed(
                        tidepool_extract_cmd::CompilerTransactionCloseEvidence::new(
                            tidepool_extract_cmd::CompilerTransactionCloseReason::Cancelled,
                            tidepool_extract_cmd::CompilerTransactionRetirement::DaemonUnobserved {
                                disconnect: None,
                            },
                        ),
                    ));
                    result
                },
            )
            .unwrap();
        match finish_runtime_targets(settled) {
            Err(RuntimeError::CompilerClose {
                primary,
                close: CompilerTransactionClose::Unconfirmed(_),
            }) => {
                assert_eq!(primary.is_some(), action_fails);
                if let Some(primary) = primary {
                    assert!(matches!(
                        *primary,
                        RuntimeError::Compile(CompileError::Diagnostics(_))
                    ));
                }
            }
            other => panic!("uncertain compiler result must refuse promotion: {other:?}"),
        }
        assert!(matches!(
            source.source_admission().admit(),
            Err(session::SourceAdmissionError::Unconfirmed { count: 1, .. })
        ));
    }
}

#[test]
fn prepared_targets_check_actual_cancellation_after_compilation() {
    tidepool_testing::eval_harness::require_extract();
    let (_root, _state, source, context) = prepared_target_source();
    let cancellation = CompilerTransactionCancellation::new();
    let cancel = cancellation.clone();
    let settled = source
        .with_source_compiler_transaction(
            artifacts::CompileWorkload::Foreground,
            cancellation,
            None,
            |_| {},
            |owner, report| {
                let prepared =
                    prepare_target_control(owner, context.clone(), PREPARED_TARGET_CONTROL, report);
                assert!(prepared.is_ok());
                cancel.cancel_host_admission();
                prepared
            },
        )
        .unwrap();
    assert!(matches!(
        settled.close(),
        CompilerTransactionClose::Clean | CompilerTransactionClose::NotStarted
    ));
    assert!(
        matches!(finish_runtime_targets(settled), Err(RuntimeError::Compile(CompileError::Io(error))) if error.kind() == std::io::ErrorKind::Interrupted)
    );
}
