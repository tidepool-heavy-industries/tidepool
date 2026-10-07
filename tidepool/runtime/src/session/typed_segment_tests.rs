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
    raw_result: Option<Arc<CaptureMutex<Vec<String>>>>,
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
        if let Some(result) = &self.raw_result {
            result.lock().push(value);
        } else {
            self.trace
                .lock()
                .push(value.parse().expect("recorded decimal Int"));
        }
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
    history_compiler_census: Option<(u64, Vec<CompilerModeObservation>)>,
    cancel_after_record: Arc<CaptureMutex<Option<Arc<std::sync::atomic::AtomicBool>>>>,
    effects: TestEffectSurface,
    images: Arc<ImageRegistry>,
    _root: tempfile::TempDir,
}

impl SemanticSession {
    fn new() -> Self {
        Self::with_raw_result(None)
    }

    fn with_raw_result(raw_result: Option<Arc<CaptureMutex<Vec<String>>>>) -> Self {
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
                raw_result,
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
            history_compiler_census: None,
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

    fn execute_history(
        &mut self,
        label: &str,
        history: &RenderedHistory,
    ) -> Result<Duration, ResidentError> {
        let submissions_before = tidepool_extract_cmd::extract_spawn_count();
        let (result, requests) = with_compiler_mode_capture(|| {
            try_execute_cell_with_template_imports_expectation(
                &mut self.resident,
                self.public,
                &self.effects,
                &self.images,
                (0, 0),
                label,
                &history.cell,
                CellDeclarationExpectation::CapturedAt {
                    name: "historyCaptured",
                    line: history.declaration_line,
                },
                &ScalePublication::Ephemeral,
                AuthorityChecks::Configured,
                &SourceImports::new(),
                None,
            )
        });
        assert_eq!(
            requests.len() as u64,
            tidepool_extract_cmd::extract_spawn_count() - submissions_before,
            "every physical compiler submission must have a typed request observation"
        );
        self.history_compiler_census = Some((submissions_before, requests));
        result.map(|(elapsed, _)| elapsed)
    }

    fn assert_no_compiler_since_last_effect(&self) {
        assert_eq!(
            self.compiler_counts.lock().last().copied(),
            Some(tidepool_extract_cmd::extract_spawn_count()),
            "execution after an effect must consume the complete admitted program"
        );
    }

    fn assert_only_publication_join_since_last_effect(&self) {
        let (submissions_before, requests) = self
            .history_compiler_census
            .as_ref()
            .expect("history retains its complete typed request census");
        assert_eq!(
            requests.len() as u64,
            tidepool_extract_cmd::extract_spawn_count() - *submissions_before,
            "no unobserved compiler request may follow history completion"
        );
        let effect_submission = self
            .compiler_counts
            .lock()
            .last()
            .copied()
            .expect("history executes its fixed final observation");
        let index = usize::try_from(effect_submission - *submissions_before).unwrap();
        let [join] = &requests[index..] else {
            panic!("only the accepted public declaration join may follow the last effect");
        };
        let published = self
            .resident
            .public_visibility_snapshot_in(self.public)
            .unwrap();
        assert!(
            join.is_publication_join(published.declaration_tip.0),
            "post-effect work must certify the exact published interface: {join:?}"
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
fn signed_template_helpers_execute_recursion_and_polymorphism_once() {
    let results = Arc::new(CaptureMutex::new(Vec::new()));
    let mut session = SemanticSession::with_raw_result(Some(results.clone()));
    let definitions = include_str!("fixtures/typed-segment-template-helpers.hs")
        .replace("__EFFECT_ROW__", session.effects.row());
    let preamble = crate::session::insert_preamble_imports(
        &crate::session::insert_preamble_imports(
            &format!("{}\n{definitions}", session.effects.preamble()),
            "qualified Control.Monad.Freer as SegmentHelperEff",
        ),
        "qualified Data.Text as SegmentHelperText",
    );
    try_execute_cell_with_template_preamble_observed(
        &mut session.resident,
        session.public,
        &session.effects,
        &session.images,
        (0, 0),
        "signed_template_helpers",
        include_str!("fixtures/typed-segment-template-helper-use.hs"),
        CellDeclarationExpectation::Total(0),
        &ScalePublication::Ephemeral,
        AuthorityChecks::SegmentWorkCounts(7),
        &SourceImports::new(),
        None,
        &preamble,
        |program| assert_eq!(program.items().len(), 7),
    )
    .unwrap();
    let observed = results.lock().clone();
    let [result] = observed.as_slice() else {
        panic!("the single authored report must execute exactly once: {observed:?}");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(result).unwrap(),
        serde_json::json!([7, 7, 3, 3.0]),
    );
    assert_eq!(session.compiler_counts.lock().len(), 1);
    session.assert_no_compiler_since_last_effect();
    for name in ["replyValue", "recursiveValue", "integerStep", "doubleStep"] {
        assert!(session
            .resident
            .current_binding_in(session.public, name)
            .is_some());
    }
}

#[test]
fn genuine_let_generalization_and_unused_inner_bottom_match_ghc() {
    let source = include_str!("fixtures/typed-segment-let-generalization.hs");
    let inline_source = source
        .replacen(
            "let segmentIdentity value = value",
            "let { segmentIdentity value = value }",
            1,
        )
        .lines()
        .collect::<Vec<_>>()
        .join("; ");
    let retained_actions = [
        "segmentRecord (segmentIdentity (17 :: Int))",
        "segmentRecord (if segmentIdentity True then 19 else 0)",
    ];
    for (cold_source, separator) in [(source, "\n"), (inline_source.as_str(), "; ")] {
        assert_eq!(ghc_trace(cold_source), [7, 11, 13]);
        let mut session = SemanticSession::new();
        session
            .execute("let_generalization", cold_source, 0)
            .unwrap();
        assert_eq!(session.observed(), [7, 11, 13]);
        session
            .execute(
                "retained_let_generalization",
                &retained_actions.join(separator),
                0,
            )
            .unwrap();
        assert_eq!(session.observed(), [7, 11, 13, 17, 19]);
        let (multiline_let, multiline_do) = if separator == "; " {
            (
                include_str!("fixtures/typed-segment-retained-layout-inline-let.hs"),
                include_str!("fixtures/typed-segment-retained-layout-inline-do.hs"),
            )
        } else {
            (
                include_str!("fixtures/typed-segment-retained-layout-newline-let.hs"),
                include_str!("fixtures/typed-segment-retained-layout-newline-do.hs"),
            )
        };
        for (name, authored, expected_trace) in [
            ("retained_multiline_let", multiline_let, [29, 32]),
            ("retained_multiline_do", multiline_do, [37, 41]),
        ] {
            assert_eq!(
                ghc_trace_with_language("", "segmentIdentity value = value", authored),
                expected_trace
            );
            session.execute(name, authored, 0).unwrap();
        }
        assert_eq!(session.observed(), [7, 11, 13, 17, 19, 29, 32, 37, 41]);
        let snapshot = session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap();
        let error = session
            .execute("invalid_statement_list", "answer <- ; segmentRecord 23", 0)
            .unwrap_err();
        assert!(
            matches!(&error, ResidentError::Session(crate::session::SessionError::Compile(crate::CompileError::Diagnostics(diagnostics))) if !diagnostics.is_empty()),
            "malformed statements must retain parser diagnostics: {error:?}"
        );
        assert_eq!(session.observed(), [7, 11, 13, 17, 19, 29, 32, 37, 41]);
        assert_eq!(
            session
                .resident
                .public_visibility_snapshot_in(session.public)
                .unwrap(),
            snapshot
        );
    }
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
    session.execute("scalar_recovery", "segmentRecord preservedScalar; valueAfterFailure <- pure (10 :: Int); segmentRecord valueAfterFailure", 0).unwrap();
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
            "demandedClosure <- pure (retainedBottom ()); segmentRecord demandedClosure",
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
        .execute("closure_recovery", "segmentRecord (23 :: Int)", 0)
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
    let source = format!(
        "demandedObservation <- pure ({observation} ()); segmentRecord demandedObservation"
    );
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
            "segmentRecord 1 >> pure (error \"effectful observation bottom\" :: Int)",
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
    let source = format!("demandedEffectfulObservation <- pure ({observation} ()); segmentRecord demandedEffectfulObservation");
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
            "segmentRecord priorTypedValue; retryTypedValue <- pure (2 :: Int); segmentRecord retryTypedValue",
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
                "cancelledValue <- segmentRecord 1 >> pure (2 :: Int); segmentRecord cancelledValue",
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
    session.execute("cancel_new_intent", "retriedValue <- segmentRecord 2 >> pure (3 :: Int); segmentRecord retriedValue; segmentRecord priorCancelledValue", 0).unwrap();
    assert_eq!(session.observed(), [1, 2, 3, 9]);
}

#[derive(Clone, Debug, serde::Serialize)]
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
    declaration_line: usize,
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
    let mut declaration_line = None;
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
                format!("segmentRecord {current}\n")
            }
            HistoryOperation::UnusedBottom => {
                coverage.unused_bottoms += 1;
                format!("let historyInner{index} = (let unused = undefined :: Int in {current})\n")
            }
            HistoryOperation::Capture => {
                captured = value;
                coverage.captures += 1;
                assert!(
                    declaration_line.replace(cell.lines().count() + 1).is_none(),
                    "a generated history retains one authored capture declaration"
                );
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
                cell.push_str("segmentRecord historyCaptured\n");
                oracle.push_str("segmentRecord historyCaptured\n");
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
        declaration_line: declaration_line.expect("history capture declaration"),
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

fn mandatory_capture_operations(seed: i8, flag: bool) -> [HistoryOperation; 11] {
    [
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
    ]
}

fn report_history_input(
    seed: i8,
    prefix: &[HistoryOperation],
    flag: bool,
    no_mr: bool,
    operations: &[HistoryOperation],
    history: &RenderedHistory,
) {
    // Emit before either oracle or native execution so shrinking failures retain
    // their complete input even when the runner's watchdog interrupts the case.
    eprintln!(
        "typed-segment-history-input {}",
        serde_json::json!({
            "seed": seed, "prefix": prefix, "flag": flag, "no_mr": no_mr,
            "operations": operations, "source": history.cell,
            "oracle_source": history.oracle, "expected": history.expected,
            "captured": history.captured, "declaration_line": history.declaration_line,
        })
    );
}

fn exercise_capture_history(history: &RenderedHistory) {
    let mut session = SemanticSession::new();
    session.execute_history("generated_cold", history).unwrap();
    session.assert_only_publication_join_since_last_effect();
    assert_eq!(session.observed(), history.expected.clone());
    session.execute_history("generated_warm", history).unwrap();
    session.assert_only_publication_join_since_last_effect();
    let mut expected = history.expected.repeat(2);
    assert_eq!(session.observed(), expected.clone());
    let before = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let error = session
        .execute(
            "generated_rejected",
            include_str!("fixtures/typed-segment-illtyped.hs"),
            1,
        )
        .unwrap_err();
    assert!(matches!(error,
        ResidentError::Session(crate::session::SessionError::Compile(crate::CompileError::Diagnostics(ref diagnostics)))
            if !diagnostics.is_empty()));
    assert_eq!(
        session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap(),
        before
    );
    assert_eq!(session.observed(), expected.clone());
    session
        .execute("generated_recovery", "segmentRecord historyCaptured", 0)
        .unwrap();
    expected.push(history.captured);
    assert_eq!(session.observed(), expected);
}

#[test]
fn deterministic_capture_history_matches_ghc_cold_warm_and_recovery() {
    // This fixed input preserves the mandatory capture/shadow and
    // rejection/recovery sequence without a random prefix.
    let operations = mandatory_capture_operations(0, false);
    let history = render_history(0, &operations, false);
    report_history_input(0, &[], false, false, &operations, &history);
    assert_eq!(history.expected, [1, 0, 1, 0]);
    assert_eq!(history.captured, 0);
    assert_ne!(history.mutated_expected, history.expected);
    assert_eq!(
        ghc_trace_with_language(
            "{-# LANGUAGE MonomorphismRestriction #-}",
            "",
            &history.oracle,
        ),
        history.expected
    );
    exercise_capture_history(&history);
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
        let mut operations = prefix.clone();
        operations.extend(mandatory_capture_operations(seed, flag));
        let history = render_history(seed, &operations, no_mr);
        report_history_input(seed, &prefix, flag, no_mr, &operations, &history);
        let language = if no_mr { "NoMonomorphismRestriction" } else { "MonomorphismRestriction" };
        let pragma = format!("{{-# LANGUAGE {language} #-}}");
        proptest::prop_assert_eq!(ghc_trace_with_language(&pragma, "", &history.oracle), history.expected.clone());
        proptest::prop_assert_ne!(&history.mutated_expected, &history.expected);
        proptest::prop_assert!(history.coverage.pure_binds > 0 && history.coverage.later_constraints > 0
            && history.coverage.tuples > 0 && history.coverage.observations > 0
            && history.coverage.unused_bottoms > 0 && history.coverage.captures > 0
            && history.coverage.shadows > 0 && history.coverage.captured_uses > 0);
        exercise_capture_history(&history);
        eprintln!("typed-segment-history {}", serde_json::json!({
            "operations": operations.len(), "coverage": history.coverage,
            "cold_runs": 1, "warm_runs": 1, "rejections": 1, "recoveries": 1,
            "oracle_sensitivity_controls": 1, "monomorphism_restriction": !no_mr,
        }));
    }
}

#[test]
fn one_four_eight_actions_use_one_completed_inference_segment() {
    use super::super::tests::TestEnvGuard;

    let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
    let _timing = TestEnvGuard::set("TIDEPOOL_TIMING", "1");
    for count in [1, 4, 8] {
        let cache = tempfile::tempdir().unwrap();
        let _cache = TestEnvGuard::set("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
        let mut session = SemanticSession::new();
        let source = (1..=count)
            .map(|ordinal| {
                let value = if ordinal == 1 {
                    "(1 :: Int)".to_owned()
                } else {
                    format!("(count{count}Value{} + 1)", ordinal - 1)
                };
                format!("count{count}Value{ordinal} <- segmentRecord {value} >> pure {value}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        try_execute_cell_with_authority_checks(
            &mut session.resident,
            session.public,
            &session.effects,
            &session.images,
            (0, 0),
            &format!("one_segment_{count}"),
            &source,
            0,
            &ScalePublication::Ephemeral,
            AuthorityChecks::SegmentWorkCounts(count),
        )
        .unwrap();
        assert_eq!(session.observed(), (1..=count as i64).collect::<Vec<_>>());
        assert_eq!(session.compiler_counts.lock().len(), count);
        session.assert_no_compiler_since_last_effect();
        for ordinal in 1..=count {
            assert!(session
                .resident
                .current_binding_in(session.public, &format!("count{count}Value{ordinal}"))
                .is_some());
        }
    }
}

#[test]
fn produced_capture_types_cross_a_declaration_barrier_without_replaying_effects() {
    let mut session = SemanticSession::new();
    let history = render_history(
        23,
        &[
            HistoryOperation::Capture,
            HistoryOperation::PureBindInt(1),
            HistoryOperation::ObserveCapture,
        ],
        false,
    );
    assert_eq!(ghc_trace(&history.oracle), [23]);
    session
        .execute_history("produced_types_across_declaration", &history)
        .unwrap();
    assert_eq!(session.observed(), [23]);
    session.assert_only_publication_join_since_last_effect();
    let published = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let declaration_surface = session
        .resident
        .exact_exports_in_namespace(
            session.public,
            tidepool_toolchain::declaration_join::ExportNamespace::Value,
            &["historyCaptured"],
        )
        .unwrap();
    assert_eq!(
        declaration_surface.source_module(),
        Some(tidepool_repr::SessionModule::lib(published.declaration_tip))
    );
    let [declaration] = declaration_surface.declarations().unwrap() else {
        panic!("published barrier must retain exactly one certified declaration export");
    };
    assert_eq!(
        declaration.kind,
        tidepool_toolchain::declaration_join::DeclarationKind::Value
    );
    assert_eq!(
        declaration.head.namespace,
        tidepool_toolchain::declaration_join::ExportNamespace::Value
    );
    assert_eq!(declaration.head.occurrence, "historyCaptured");
    assert!(session
        .resident
        .current_binding_in(session.public, "historyValue0")
        .is_some());
    session
        .execute("produced_type_reuse", "segmentRecord historyCaptured", 0)
        .unwrap();
    assert_eq!(session.observed(), [23, 23]);
    session.assert_no_compiler_since_last_effect();
}

#[test]
fn warm_target_selects_previously_unselected_original_native_groups() {
    use std::collections::BTreeSet;
    use tidepool_toolchain::artifact_inventory::{
        ArtifactKind, NativeGroupKey, NativeRequirementRoot,
    };

    fn execution_context(
        item: &tidepool_toolchain::checked_cell::CellProgramItem,
    ) -> Arc<tidepool_toolchain::declaration_join::ExactDeclarationContext> {
        let native = item.native().unwrap();
        let (table, _) =
            tidepool_repr::serial::read_metadata(item.native_metadata_bytes().unwrap()).unwrap();
        let turn: ciborium::value::Value =
            ciborium::de::from_reader(item.native_turn_bytes().unwrap()).unwrap();
        let fields = turn.as_array().unwrap()[1].as_array().unwrap();
        let sites = tidepool_toolchain::artifacts::decode_turn_yield_sites(&fields[3]).unwrap();
        native
            .original_execution_context(&native.target_owned(), &table, &sites)
            .unwrap()
    }

    let mut session = SemanticSession::new();
    let mut cold = None;
    try_execute_cell_with_template_imports_expectation_observed(
        &mut session.resident,
        session.public,
        &session.effects,
        &session.images,
        (0, 0),
        "warm_native_cold_baseline",
        "baselinePlannedValue <- pure (Carrier.carrierBaseline 2)",
        CellDeclarationExpectation::Total(0),
        &ScalePublication::Ephemeral,
        AuthorityChecks::Configured,
        &SourceImports::from_specs(["qualified SegmentProbeSupport as Carrier"]),
        None,
        |program| {
            let [item] = program.items() else {
                panic!("the cold baseline must admit exactly one native binding");
            };
            cold = Some((
                execution_context(item),
                item.native()
                    .unwrap()
                    .value_interface_certificate()
                    .unwrap(),
            ));
        },
    )
    .unwrap();
    assert!(session.observed().is_empty());
    let (before, baseline_proof) = cold.unwrap();
    let baseline_binding = session
        .resident
        .current_binding_in(session.public, "baselinePlannedValue")
        .unwrap();
    assert_eq!(baseline_binding.1, baseline_proof.owner());
    assert!(Arc::ptr_eq(
        session
            .resident
            .retained_checked_value_artifact(baseline_binding.1)
            .unwrap(),
        &baseline_proof,
    ));
    let originals = before.recovery_products();
    let original = originals
        .iter()
        .find(|product| product.owner().module == "SegmentProbeSupport")
        .expect("the real cold compiler must retain the complete probe carrier");
    let descriptor = before
        .artifact_view()
        .descriptors()
        .into_iter()
        .find(|descriptor| {
            descriptor.kind == ArtifactKind::OriginalModule
                && descriptor.owner.unit == original.owner().unit
                && descriptor.owner.module == original.owner().module
        })
        .unwrap();
    assert_eq!(
        descriptor.product_sha256,
        Some(sha2::Sha256::digest(original.product_bytes()).into())
    );
    let full = tidepool_repr::execution_schema::parse_module_products(
        original.product_bytes(),
        &tidepool_toolchain::prepared_artifact::production_requirements().unwrap(),
        tidepool_repr::execution_schema::InventoryDecodeLimits::default(),
    )
    .unwrap();
    let [full] = full.as_slice() else {
        panic!("an original native carrier must contain exactly one owner")
    };
    let operation_group = |name: &str| {
        full.groups
            .iter()
            .find(|group| {
                group.binders().iter().any(|binder| {
                    binder.module == "SegmentProbeSupport" && binder.occurrence == name
                })
            })
            .unwrap()
            .original_ordinal()
    };
    let baseline = NativeGroupKey {
        artifact: descriptor.id,
        original_ordinal: operation_group("carrierBaseline"),
    };
    let increment = NativeGroupKey {
        artifact: descriptor.id,
        original_ordinal: operation_group("lateCarrierIncrement"),
    };
    let double = NativeGroupKey {
        artifact: descriptor.id,
        original_ordinal: operation_group("lateCarrierDouble"),
    };
    let negate = NativeGroupKey {
        artifact: descriptor.id,
        original_ordinal: operation_group("lateCarrierNegate"),
    };
    assert_ne!(increment, double);
    assert_ne!(increment, negate);
    assert_ne!(double, negate);
    assert_ne!(baseline, increment);
    assert_ne!(baseline, double);
    assert_ne!(baseline, negate);
    let mut selected_carrier = before
        .artifact_view()
        .selected_native_groups()
        .into_iter()
        .filter(|key| key.artifact == descriptor.id)
        .collect::<BTreeSet<_>>();
    assert!(selected_carrier.contains(&baseline));
    assert!(!selected_carrier.contains(&increment));
    assert!(!selected_carrier.contains(&double));
    assert!(!selected_carrier.contains(&negate));
    let initially_selected_carrier = selected_carrier.clone();
    let old_owners = originals
        .iter()
        .map(|product| (product.owner().unit.clone(), product.owner().module.clone()))
        .collect::<BTreeSet<_>>();
    let assert_retained =
        |context: &tidepool_toolchain::declaration_join::ExactDeclarationContext| {
            let retained = context.recovery_products();
            let retained = retained
                .iter()
                .find(|product| product.owner().module == original.owner().module)
                .unwrap();
            assert_eq!(retained.owner(), original.owner());
            assert_eq!(retained.product_bytes(), original.product_bytes());
            assert!(context.artifact_view().descriptors().contains(&descriptor));
        };
    let mut observed = 0;
    let mut warm = None;
    try_execute_cell_with_template_imports_expectation_observed(
        &mut session.resident,
        session.public,
        &session.effects,
        &session.images,
        (0, 0),
        "warm_native_carrier_demand",
        include_str!("fixtures/typed-segment-warm-carrier.hs"),
        CellDeclarationExpectation::Total(0),
        &ScalePublication::Ephemeral,
        AuthorityChecks::NativeEmissionOwnersAbsent(&old_owners),
        &SourceImports::from_specs(["qualified SegmentProbeSupport as Carrier"]),
        None,
        |program| {
            for item in program.items() {
                let Some(native) = item.native() else {
                    continue;
                };
                observed += 1;
                let products = item.native_products().unwrap();
                let context = execution_context(item);
                assert_retained(&context);
                if native.item().binders() == ["warmCarrierResult"] {
                    assert!(warm.is_none());
                    warm = Some((
                        context.clone(),
                        native.value_interface_certificate().unwrap(),
                    ));
                }
                let NativeRequirementRoot::Group {
                    artifact,
                    original_ordinal,
                } = native.typed_entry().unwrap().native_requirement_root()
                else {
                    panic!("the warm item must issue an exact native group root");
                };
                let selected = products.artifact_view.selected_native_groups();
                assert!(selected.contains(&NativeGroupKey {
                    artifact,
                    original_ordinal
                }));
                let carrier_groups = selected
                    .into_iter()
                    .filter(|key| key.artifact == descriptor.id)
                    .collect::<BTreeSet<_>>();
                assert!(selected_carrier.is_subset(&carrier_groups));
                selected_carrier = carrier_groups;
            }
        },
    )
    .unwrap();
    assert!(observed >= 2, "both warm authored actions must be observed");
    assert!(selected_carrier.contains(&increment));
    assert!(selected_carrier.contains(&double));
    assert!(!selected_carrier.contains(&negate));
    assert!(selected_carrier.len() < full.groups.len());
    let (after, result_proof) = warm.unwrap();
    let result_binding = session
        .resident
        .current_binding_in(session.public, "warmCarrierResult")
        .unwrap();
    assert_eq!(result_binding.1, result_proof.owner());
    assert!(Arc::ptr_eq(
        session
            .resident
            .retained_checked_value_artifact(result_binding.1)
            .unwrap(),
        &result_proof,
    ));
    assert_eq!(
        session
            .resident
            .current_binding_in(session.public, "baselinePlannedValue")
            .unwrap(),
        baseline_binding,
    );
    assert_retained(&after);
    let result_selected_carrier = after
        .artifact_view()
        .selected_native_groups()
        .into_iter()
        .filter(|key| key.artifact == descriptor.id)
        .collect::<BTreeSet<_>>();
    assert!(initially_selected_carrier.is_subset(&result_selected_carrier));
    assert!(result_selected_carrier.contains(&increment));
    assert!(result_selected_carrier.contains(&double));
    assert!(!result_selected_carrier.contains(&negate));
    assert!(result_selected_carrier.len() < full.groups.len());
    assert!(result_selected_carrier.is_subset(&selected_carrier));
    assert_eq!(session.observed(), [7]);
    session.assert_no_compiler_since_last_effect();
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
    let source = include_str!("fixtures/typed-segment-native-entry.hs");
    let line = source
        .lines()
        .position(|line| line == "boundaryCapture :: Int")
        .expect("authored capture signature")
        + 1;
    try_execute_cell_with_template_imports_expectation(
        &mut session.resident,
        session.public,
        &session.effects,
        &session.images,
        (0, 0),
        "native_entry_refusal",
        source,
        CellDeclarationExpectation::CapturedAt {
            name: "boundaryCapture",
            line,
        },
        &ScalePublication::Ephemeral,
        AuthorityChecks::TypedEntryRefusalBranches,
        &SourceImports::new(),
        None,
    )
    .unwrap();
    assert_eq!(session.observed(), [4]);
}

#[test]
fn zero_capture_let_executes_without_publishing_a_dummy_binding() {
    let inline = include_str!("fixtures/typed-segment-zero-let-inline.hs");
    let newline = include_str!("fixtures/typed-segment-zero-let-newline.hs");
    assert_eq!(ghc_trace(inline), [1, 2]);
    assert_eq!(ghc_trace(newline), [1, 2]);
    let invalid = ghc_oracle_with_language(
        "",
        "",
        include_str!("fixtures/typed-segment-zero-let-invalid-layout.hs"),
    );
    assert!(!invalid.status.success());
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("parse error"),
        "the implicit inline let must fail GHC's grammar: {}",
        String::from_utf8_lossy(&invalid.stderr)
    );
    let mut session = SemanticSession::new();
    let before = session.resident.binding_names_in(session.public);
    session
        .execute("zero_let", "let _ = (undefined :: Int)", 0)
        .unwrap();
    assert_eq!(session.resident.binding_names_in(session.public), before);
    assert!(session.observed().is_empty());
    session.execute("zero_let_order", inline, 0).unwrap();
    assert_eq!(session.observed(), [1, 2]);
}

#[test]
fn zero_capture_bang_let_preserves_forcing_and_prior_effects() {
    let mut session = SemanticSession::new();
    let before = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    assert_eq!(before.machine_incarnation, None);
    let error = session.execute("zero_bang_let", "{-# LANGUAGE BangPatterns #-}\nsegmentRecord 1\nlet !_ = (error \"strict discarded let\" :: Int)\nsegmentRecord 2", 1).unwrap_err();
    assert!(
        is_raised_exception(&error),
        "the native strict wildcard must actually force: {error:?}"
    );
    assert_eq!(session.observed(), [1]);
    session.assert_no_compiler_since_last_effect();
    let failed = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    let incarnation = failed
        .machine_incarnation
        .expect("first native execution establishes a machine even when its cell fails");
    let mut initialized = before;
    initialized.machine_incarnation = Some(incarnation);
    assert_eq!(failed, initialized);
    session
        .execute("zero_bang_retry", "segmentRecord (3 :: Int)", 0)
        .unwrap();
    assert_eq!(session.observed(), [1, 3]);
    assert_eq!(
        session
            .resident
            .public_visibility_snapshot_in(session.public)
            .unwrap()
            .machine_incarnation,
        Some(incarnation)
    );
}

#[test]
fn zero_capture_action_runs_once_before_the_next_item() {
    let mut session = SemanticSession::new();
    session
        .execute("zero_action", "_ <- segmentRecord 1; segmentRecord 2", 0)
        .unwrap();
    assert_eq!(session.observed(), [1, 2]);
    session.assert_no_compiler_since_last_effect();
}

#[test]
fn strict_let_group_forces_before_retaining_its_closure_capture() {
    let mut session = SemanticSession::new();
    session
        .execute(
            "strict_closure_prior",
            "let preservedStrictValue = (3 :: Int)",
            0,
        )
        .unwrap();
    let before = session
        .resident
        .public_visibility_snapshot_in(session.public)
        .unwrap();
    assert!(before.machine_incarnation.is_some());
    let error = session
        .execute(
            "strict_closure_let",
            include_str!("fixtures/typed-segment-strict-closure-let.hs"),
            1,
        )
        .unwrap_err();
    assert!(
        is_raised_exception(&error),
        "strict let group must force before returning its closure: {error:?}"
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
        .execute(
            "strict_closure_retry",
            "segmentRecord preservedStrictValue",
            0,
        )
        .unwrap();
    assert_eq!(session.observed(), [1, 3]);
}
