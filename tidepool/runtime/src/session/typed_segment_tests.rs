//! Semantic observations through admitted compilation, native execution and publication.

use super::*;
use tidepool_bridge_derive::FromHaskell;
use tidepool_effect::dispatch::{EffectContext, EffectHandler, Response};
use tidepool_effect::EffectError;

#[derive(FromHaskell)]
enum ProbeRequest {
    #[haskell(name = "Print")]
    Print(String),
}

struct ProbeHandler {
    trace: Arc<CaptureMutex<Vec<i64>>>,
    compiler_counts: Arc<CaptureMutex<Vec<u64>>>,
    cancel_after_record: Arc<CaptureMutex<Option<Arc<std::sync::atomic::AtomicBool>>>>,
}

impl EffectHandler<QuietOutput> for ProbeHandler {
    type Request = ProbeRequest;

    fn handle(
        &mut self,
        request: ProbeRequest,
        cx: &EffectContext<'_, QuietOutput>,
    ) -> Result<Response, EffectError> {
        let ProbeRequest::Print(value) = request;
        self.trace
            .lock()
            .push(value.parse().expect("recorded decimal Int"));
        self.compiler_counts
            .lock()
            .push(tidepool_extract_cmd::extract_spawn_count());
        if let Some(cancel) = self.cancel_after_record.lock().take() {
            cancel.store(true, std::sync::atomic::Ordering::Release);
        }
        cx.respond(())
    }
}

type ProbeSession = ScaleSession<frunk::HList!(ProbeHandler)>;

struct SemanticSession {
    resident: ProbeSession,
    public: ScopeId,
    trace: Arc<CaptureMutex<Vec<i64>>>,
    compiler_counts: Arc<CaptureMutex<Vec<u64>>>,
    cancel_after_record: Arc<CaptureMutex<Option<Arc<std::sync::atomic::AtomicBool>>>>,
    effects: TestEffectSurface,
    images: Arc<ImageRegistry>,
    _root: tempfile::TempDir,
}

impl SemanticSession {
    fn new() -> Self {
        tidepool_testing::eval_harness::require_extract();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("SegmentProbeSupport.hs"),
            include_str!("fixtures/typed-segment-probe-support.hs"),
        )
        .unwrap();
        let effects = TestEffectSurface::with_options(
            &[tidepool_mcp::console_decl()],
            tidepool_testing::effect_surface::TestEffectSurfaceOptions {
                row_args: tidepool_mcp::RowArgs::default().importing(["SegmentProbeSupport"]),
                ..Default::default()
            },
        )
        .unwrap();
        let images = Arc::new(ImageRegistry::new());
        let lib = SessionLib::open(
            SessionId(1010),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(effects.include_paths().to_vec());
        let mut persistent = PersistentSession::new(Some(lib), 1024 * 1024);
        persistent.set_image_registry(images.clone());
        let public = persistent.mint_scope(ScopeId::ROOT).unwrap();
        let trace = Arc::new(CaptureMutex::new(Vec::new()));
        let compiler_counts = Arc::new(CaptureMutex::new(Vec::new()));
        let cancel_after_record = Arc::new(CaptureMutex::new(None));
        let resident = ResidentSession::from_persistent_for_test(
            frunk::hlist![ProbeHandler {
                trace: trace.clone(),
                compiler_counts: compiler_counts.clone(),
                cancel_after_record: cancel_after_record.clone()
            }],
            QuietOutput,
            persistent,
        );
        Self {
            resident,
            public,
            trace,
            compiler_counts,
            cancel_after_record,
            effects,
            images,
            _root: root,
        }
    }

    fn execute(
        &mut self,
        label: &str,
        source: &str,
        declarations: usize,
    ) -> Result<Duration, ResidentError> {
        try_execute_cell_with_authority_checks(
            &mut self.resident,
            self.public,
            &self.effects,
            &self.images,
            (0, 0),
            label,
            source,
            declarations,
            &ScalePublication::Ephemeral,
            AuthorityChecks::Configured,
        )
    }

    fn observed(&self) -> Vec<i64> {
        self.trace.lock().clone()
    }

    fn assert_no_compiler_since_last_effect(&self) {
        assert_eq!(
            self.compiler_counts.lock().last().copied(),
            Some(tidepool_extract_cmd::extract_spawn_count()),
            "execution after an effect must consume the complete admitted program"
        );
    }
}

fn ghc_trace(source: &str) -> Vec<i64> {
    ghc_trace_with_declarations("", source)
}

fn ghc_trace_with_declarations(declarations: &str, source: &str) -> Vec<i64> {
    ghc_trace_with_language("", declarations, source)
}

#[allow(
    clippy::disallowed_methods,
    reason = "bounded independent oracle from the native target's declared toolchain"
)]
fn ghc_oracle_with_language(
    language: &str,
    declarations: &str,
    source: &str,
) -> std::process::Output {
    let root = tempfile::tempdir().unwrap();
    let body = source
        .lines()
        .map(|line| format!("  {line}\n"))
        .collect::<String>();
    let source = include_str!("fixtures/typed-segment-oracle.hs")
        .replace("__LANGUAGE__", language)
        .replace("__DECLARATIONS__", declarations)
        .replace("__BODY__", &body);
    let path = root.path().join("SegmentOracle.hs");
    std::fs::write(&path, source).unwrap();
    let ghc = std::env::var_os("TIDEPOOL_GHC")
        .expect("native semantic target must declare its exact GHC executable");
    let libdir = std::env::var("TIDEPOOL_GHC_LIBDIR")
        .expect("native semantic target must declare its pinned GHC oracle");
    std::process::Command::new(ghc)
        .env_remove("GHC_PACKAGE_PATH")
        .env_remove("GHC_ENVIRONMENT")
        .current_dir(root.path())
        .arg(format!("-B{libdir}"))
        .arg(&path)
        .args([
            "-clear-package-db",
            "-global-package-db",
            "-no-user-package-db",
            "-hide-all-packages",
            "-package",
            "base",
            "-package-env",
            "-",
            "-v0",
            "-e",
            "SegmentOracle.main",
        ])
        .output()
        .expect("execute the declared GHC oracle")
}

fn ghc_trace_with_language(language: &str, declarations: &str, source: &str) -> Vec<i64> {
    let oracle = ghc_oracle_with_language(language, declarations, source);
    assert!(
        oracle.status.success(),
        "GHC rejected semantic oracle: {}",
        String::from_utf8_lossy(&oracle.stderr)
    );
    serde_json::from_slice(&oracle.stdout).expect("GHC integer trace")
}

fn is_raised_exception(error: &ResidentError) -> bool {
    matches!(error,
        ResidentError::Prepared(PreparedRuntimeError::Run(
            tidepool_codegen::prepared_program::ExecutionError::Runtime(failure)
        )) if matches!(failure.cause,
            tidepool_codegen::host_fns::RuntimeError::RaisedException
            | tidepool_codegen::host_fns::RuntimeError::RaisedExceptionMessage(_)))
}

#[test]
fn genuine_let_generalization_and_unused_inner_bottom_match_ghc() {
    let source = include_str!("fixtures/typed-segment-let-generalization.hs");
    assert_eq!(ghc_trace(source), [7, 11, 13]);
    let mut session = SemanticSession::new();
    session.execute("let_generalization", source, 0).unwrap();
    assert_eq!(session.observed(), [7, 11, 13]);
    session
        .execute(
            "retained_let_generalization",
            "record (segmentIdentity (17 :: Int)); record (if segmentIdentity True then 19 else 0)",
            0,
        )
        .unwrap();
    assert_eq!(session.observed(), [7, 11, 13, 17, 19]);
}

#[test]
fn scalar_publication_forces_after_effect_without_publishing_then_recovers() {
    let mut session = SemanticSession::new();
    session
        .execute("scalar_prior", "let preservedScalar = (9 :: Int)", 0)
        .unwrap();
    let before = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let error = session
        .execute(
            "scalar_bottom",
            include_str!("fixtures/typed-segment-strict-publication.hs"),
            0,
        )
        .unwrap_err();
    assert!(
        is_raised_exception(&error),
        "the actual scalar forcing failure is preserved: {error:?}"
    );
    assert_eq!(session.observed(), [1]);
    session.assert_no_compiler_since_last_effect();
    assert_eq!(
        session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap(),
        before
    );
    session.execute("scalar_recovery", "record preservedScalar; valueAfterFailure <- pure (10 :: Int); record valueAfterFailure", 0).unwrap();
    assert_eq!(session.observed(), [1, 9, 10]);
}

#[test]
fn retained_action_closure_demand_does_not_repeat_completed_effect() {
    let mut session = SemanticSession::new();
    session
        .execute(
            "retain_closure",
            include_str!("fixtures/typed-segment-retained-bottom.hs"),
            0,
        )
        .unwrap();
    assert_eq!(session.observed(), [1]);
    let before = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let error = session
        .execute(
            "demand_closure",
            "demandedClosure <- pure (retainedBottom ()); record demandedClosure",
            0,
        )
        .unwrap_err();
    assert!(
        is_raised_exception(&error),
        "later closure demand preserves the actual exception: {error:?}"
    );
    assert_eq!(session.observed(), [1]);
    assert_eq!(
        session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap(),
        before
    );
    session
        .execute("closure_recovery", "record (23 :: Int)", 0)
        .unwrap();
    assert_eq!(session.observed(), [1, 23]);
}

#[test]
fn bare_observation_retains_lazy_thunk_before_later_demand() {
    let mut session = SemanticSession::new();
    let before = session.resident.binding_names_in(session.public);
    session
        .execute(
            "bare_bottom",
            "error \"retained observation bottom\" :: Int",
            0,
        )
        .unwrap();
    assert!(session.observed().is_empty());
    let after = session.resident.binding_names_in(session.public);
    let added = after
        .iter()
        .filter(|name| !before.contains(name))
        .collect::<Vec<_>>();
    let [observation] = added.as_slice() else {
        panic!("one actual observation binder must be published: {added:?}");
    };
    let snapshot = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let source =
        format!("demandedObservation <- pure ({observation} ()); record demandedObservation");
    let error = session
        .execute("demand_observation", &source, 0)
        .unwrap_err();
    assert!(
        is_raised_exception(&error),
        "later observation demand preserves the actual exception: {error:?}"
    );
    assert!(session.observed().is_empty());
    assert_eq!(
        session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap(),
        snapshot
    );
}

#[test]
fn effectful_observation_runs_once_and_retains_its_lazy_result() {
    let mut session = SemanticSession::new();
    let before = session.resident.binding_names_in(session.public);
    session
        .execute(
            "effectful_observation",
            "record 1 >> pure (error \"effectful observation bottom\" :: Int)",
            0,
        )
        .unwrap();
    assert_eq!(session.observed(), [1]);
    let after = session.resident.binding_names_in(session.public);
    let added = after
        .iter()
        .filter(|name| !before.contains(name))
        .collect::<Vec<_>>();
    let [observation] = added.as_slice() else {
        panic!("one actual observation binder must be published: {added:?}");
    };
    let snapshot = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let source = format!("demandedEffectfulObservation <- pure ({observation} ()); record demandedEffectfulObservation");
    let error = session
        .execute("demand_effectful_observation", &source, 0)
        .unwrap_err();
    assert!(
        is_raised_exception(&error),
        "later demand must preserve the language failure: {error:?}"
    );
    assert_eq!(session.observed(), [1]);
    assert_eq!(
        session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap(),
        snapshot
    );
}

#[test]
fn late_type_error_prevents_all_effects_and_publication_before_new_retry() {
    let mut session = SemanticSession::new();
    session
        .execute("compile_prior", "let priorTypedValue = (9 :: Int)", 0)
        .unwrap();
    let before = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let error = session
        .execute(
            "compile_refusal",
            include_str!("fixtures/typed-segment-illtyped.hs"),
            1,
        )
        .unwrap_err();
    assert!(
        matches!(&error, ResidentError::Session(crate::session::SessionError::Compile(crate::CompileError::Diagnostics(diagnostics))) if !diagnostics.is_empty()),
        "the real GHC refusal must survive: {error:?}"
    );
    assert!(session.observed().is_empty());
    assert_eq!(
        session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap(),
        before
    );
    session
        .execute(
            "compile_retry",
            "record priorTypedValue; retryTypedValue <- pure (2 :: Int); record retryTypedValue",
            0,
        )
        .unwrap();
    assert_eq!(session.observed(), [9, 2]);
}

#[test]
fn cancellation_after_real_effect_preserves_receipt_and_allows_new_intent() {
    use std::sync::atomic::AtomicBool;
    let mut session = SemanticSession::new();
    session
        .execute("cancel_prior", "let priorCancelledValue = (9 :: Int)", 0)
        .unwrap();
    let before = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    *session.cancel_after_record.lock() = Some(cancel.clone());
    let error = session
        .resident
        .with_invocation_cancel(cancel, |resident| {
            try_execute_cell_with_authority_checks(
                resident,
                session.public,
                &session.effects,
                &session.images,
                (0, 0),
                "cancel_after_effect",
                "cancelledValue <- record 1 >> pure (2 :: Int); record cancelledValue",
                0,
                &ScalePublication::Ephemeral,
                AuthorityChecks::Configured,
            )
        })
        .unwrap_err();
    assert!(
        matches!(&error, ResidentError::Prepared(error) if error.kind() == crate::session::PreparedFailureKind::Cancelled),
        "cancellation must not be an integrity or language failure: {error:?}"
    );
    assert_eq!(session.observed(), [1]);
    session.assert_no_compiler_since_last_effect();
    assert_eq!(
        session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap(),
        before
    );
    session.execute("cancel_new_intent", "retriedValue <- record 2 >> pure (3 :: Int); record retriedValue; record priorCancelledValue", 0).unwrap();
    assert_eq!(session.observed(), [1, 2, 3, 9]);
}

#[derive(Clone, Debug)]
enum HistoryOperation {
    LetInt(i8),
    PureBindInt(i8),
    ReadInt(i8),
    Tuple(i8, bool),
    Observe,
    UnusedBottom,
    Capture,
    Shadow,
    ObserveCapture,
}

#[derive(Default, Debug, serde::Serialize)]
struct HistoryCoverage {
    pure_binds: usize,
    later_constraints: usize,
    tuples: usize,
    observations: usize,
    unused_bottoms: usize,
    captures: usize,
    shadows: usize,
    captured_uses: usize,
}

struct RenderedHistory {
    cell: String,
    oracle: String,
    mutated_expected: Vec<i64>,
    expected: Vec<i64>,
    captured: i64,
    coverage: HistoryCoverage,
}

fn render_history(seed: i8, operations: &[HistoryOperation], no_mr: bool) -> RenderedHistory {
    // The reference state retains values directly. It does not reuse compiler
    // binder identities, artifact roots, source selection or capture machinery.
    let mut value = i64::from(seed);
    let mut captured = value;
    let mut current = "historyValue0".to_owned();
    let initial = format!("let {current} = ({value} :: Int)\n");
    let language = if no_mr {
        "NoMonomorphismRestriction"
    } else {
        "MonomorphismRestriction"
    };
    let mut cell = format!("{{-# LANGUAGE {language} #-}}\n{initial}");
    let mut oracle = initial;
    let mut mutated_expected = Vec::new();
    let mut expected = Vec::new();
    let mut coverage = HistoryCoverage::default();
    for (index, operation) in operations.iter().enumerate() {
        let fresh = format!("historyValue{}", index + 1);
        let line = match operation {
            HistoryOperation::LetInt(next) => {
                value = i64::from(*next);
                current = fresh;
                format!("let {current} = ({value} :: Int)\n")
            }
            HistoryOperation::PureBindInt(next) => {
                value = i64::from(*next);
                current = fresh;
                coverage.pure_binds += 1;
                format!("{current} <- pure ({value} :: Int)\n")
            }
            HistoryOperation::ReadInt(next) => {
                value = i64::from(*next) + 1;
                current = fresh;
                coverage.later_constraints += 1;
                format!("historyRead{index} <- pure (read \"{next}\")\nlet {current} = historyRead{index} + (1 :: Int)\n")
            }
            HistoryOperation::Tuple(next, flag) => {
                value = i64::from(*next) + i64::from(*flag);
                current = fresh;
                coverage.tuples += 1;
                let flag = if *flag { "True" } else { "False" };
                format!("(historyTuple{index}, historyFlag{index}) <- pure (({next} :: Int), {flag})\nlet {current} = historyTuple{index} + (if historyFlag{index} then 1 else 0)\n")
            }
            HistoryOperation::Observe => {
                expected.push(value);
                mutated_expected.push(value);
                coverage.observations += 1;
                format!("record {current}\n")
            }
            HistoryOperation::UnusedBottom => {
                coverage.unused_bottoms += 1;
                format!("let historyInner{index} = (let unused = undefined :: Int in {current})\n")
            }
            HistoryOperation::Capture => {
                captured = value;
                coverage.captures += 1;
                cell.push_str(&format!(
                    "historyCaptured :: Int\nhistoryCaptured = {current}\n"
                ));
                let line = format!("let historyCaptured = {current}\n");
                oracle.push_str(&line);
                continue;
            }
            HistoryOperation::Shadow => {
                value += 1;
                coverage.shadows += 1;
                format!("let {current} = ({value} :: Int)\n")
            }
            HistoryOperation::ObserveCapture => {
                expected.push(captured);
                coverage.captured_uses += 1;
                cell.push_str("record historyCaptured\n");
                oracle.push_str("record historyCaptured\n");
                // A plausible capture bug resolves the most recent same-spelled
                // value rather than the original declaration's lexical value.
                mutated_expected.push(value);
                continue;
            }
        };
        cell.push_str(&line);
        oracle.push_str(&line);
    }
    RenderedHistory {
        cell,
        oracle,
        mutated_expected,
        expected,
        captured,
        coverage,
    }
}

fn history_operation_strategy() -> impl proptest::strategy::Strategy<Value = HistoryOperation> {
    use proptest::prelude::*;
    prop_oneof![
        (-3_i8..4).prop_map(HistoryOperation::LetInt),
        (-3_i8..4).prop_map(HistoryOperation::PureBindInt),
        (-3_i8..4).prop_map(HistoryOperation::ReadInt),
        (-3_i8..4, any::<bool>()).prop_map(|(value, flag)| HistoryOperation::Tuple(value, flag)),
        Just(HistoryOperation::Observe),
        Just(HistoryOperation::UnusedBottom),
    ]
}

fn history_config() -> proptest::test_runner::Config {
    use proptest::test_runner::{Config, FileFailurePersistence};
    let mut config = Config::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 3;
    }
    config.max_shrink_iters = 16;
    config.source_file = Some(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/session/typed_segment_tests.rs"
    ));
    if let Some(path) = option_env!("TIDEPOOL_PROPTEST_REGRESSIONS") {
        config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(path)));
    }
    config
}

proptest::proptest! {
    #![proptest_config(history_config())]

    #[test]
    fn generated_capture_histories_match_ghc_cold_warm_and_recovery(
        seed in -3_i8..4,
        prefix in proptest::collection::vec(history_operation_strategy(), 0..4),
        flag in proptest::bool::ANY,
        no_mr in proptest::bool::ANY,
    ) {
        let mut operations = prefix;
        operations.extend([
            HistoryOperation::ReadInt(seed),
            HistoryOperation::Observe,
            HistoryOperation::PureBindInt(seed),
            HistoryOperation::Tuple(seed, flag),
            HistoryOperation::UnusedBottom,
            HistoryOperation::Capture,
            HistoryOperation::ObserveCapture,
            HistoryOperation::UnusedBottom,
            HistoryOperation::Shadow,
            HistoryOperation::Observe,
            HistoryOperation::ObserveCapture,
        ]);
        let history = render_history(seed, &operations, no_mr);
        let language = if no_mr { "NoMonomorphismRestriction" } else { "MonomorphismRestriction" };
        let pragma = format!("{{-# LANGUAGE {language} #-}}");
        proptest::prop_assert_eq!(ghc_trace_with_language(&pragma, "", &history.oracle), history.expected.clone());
        proptest::prop_assert_ne!(&history.mutated_expected, &history.expected);
        proptest::prop_assert!(history.coverage.pure_binds > 0 && history.coverage.later_constraints > 0
            && history.coverage.tuples > 0 && history.coverage.observations > 0
            && history.coverage.unused_bottoms > 0 && history.coverage.captures > 0
            && history.coverage.shadows > 0 && history.coverage.captured_uses > 0);
        let mut session = SemanticSession::new();
        session.execute("generated_cold", &history.cell, 1).unwrap();
        proptest::prop_assert_eq!(session.observed(), history.expected.clone());
        session.execute("generated_warm", &history.cell, 1).unwrap();
        let mut expected = history.expected.repeat(2);
        proptest::prop_assert_eq!(session.observed(), expected.clone());
        let before = session.resident.public_visibility_snapshot_in(session.public).unwrap();
        let error = session.execute("generated_rejected", include_str!("fixtures/typed-segment-illtyped.hs"), 1).unwrap_err();
        proptest::prop_assert!(matches!(error,
            ResidentError::Session(crate::session::SessionError::Compile(crate::CompileError::Diagnostics(ref diagnostics)))
                if !diagnostics.is_empty()));
        proptest::prop_assert_eq!(session.resident.public_visibility_snapshot_in(session.public).unwrap(), before);
        proptest::prop_assert_eq!(session.observed(), expected.clone());
        session.execute("generated_recovery", "record historyCaptured", 0).unwrap();
        expected.push(history.captured);
        proptest::prop_assert_eq!(session.observed(), expected);
        eprintln!("typed-segment-history {}", serde_json::json!({
            "operations": operations.len(), "coverage": history.coverage,
            "cold_runs": 1, "warm_runs": 1, "rejections": 1, "recoveries": 1,
            "oracle_sensitivity_controls": 1, "monomorphism_restriction": !no_mr,
        }));
    }
}

#[test]
fn authentic_native_entries_refuse_root_and_order_substitution_before_effects() {
    let mut session = SemanticSession::new();
    session
        .execute(
            "native_entry_baseline",
            "baselinePlannedValue <- pure (2 :: Int)",
            0,
        )
        .unwrap();
    try_execute_cell_with_authority_checks(
        &mut session.resident, session.public, &session.effects, &session.images, (0, 0),
        "native_entry_refusal", "firstPlannedValue <- pure (baselinePlannedValue + 1); secondPlannedValue <- pure (firstPlannedValue + 1); record secondPlannedValue", 0,
        &ScalePublication::Ephemeral, AuthorityChecks::TypedEntryRefusalBranches,
    ).unwrap();
    assert_eq!(session.observed(), [4]);
}

#[test]
fn zero_capture_let_executes_without_publishing_a_dummy_binding() {
    let mut session = SemanticSession::new();
    let before = session.resident.binding_names_in(session.public);
    session
        .execute("zero_let", "let _ = (undefined :: Int)", 0)
        .unwrap();
    assert_eq!(session.resident.binding_names_in(session.public), before);
    assert!(session.observed().is_empty());
    session
        .execute(
            "zero_let_order",
            "record 1; let _ = (undefined :: Int); record 2",
            0,
        )
        .unwrap();
    assert_eq!(session.observed(), [1, 2]);
}

#[test]
fn zero_capture_bang_let_preserves_forcing_and_prior_effects() {
    let mut session = SemanticSession::new();
    let before = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let error = session.execute("zero_bang_let", "{-# LANGUAGE BangPatterns #-}\nrecord 1\nlet !_ = (error \"strict discarded let\" :: Int)\nrecord 2", 1).unwrap_err();
    assert!(
        is_raised_exception(&error),
        "the native strict wildcard must actually force: {error:?}"
    );
    assert_eq!(session.observed(), [1]);
    session.assert_no_compiler_since_last_effect();
    assert_eq!(
        session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap(),
        before
    );
    session
        .execute("zero_bang_retry", "record (3 :: Int)", 0)
        .unwrap();
    assert_eq!(session.observed(), [1, 3]);
}

#[test]
fn zero_capture_action_runs_once_before_the_next_item() {
    let mut session = SemanticSession::new();
    session
        .execute("zero_action", "_ <- record 1; record 2", 0)
        .unwrap();
    assert_eq!(session.observed(), [1, 2]);
    session.assert_no_compiler_since_last_effect();
}
