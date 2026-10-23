//! Semantic observations through admitted compilation, native execution and publication.

use super::*;
use crate::session::resident::world_cell_test_support::{
    bootstrap_world_for, fixture_cell_recipe, plan_fixture_specification_for,
    source_action_with_original, InputRecipe,
};
use tidepool_bridge_derive::FromHaskell;
use tidepool_effect::dispatch::{EffectContext, EffectHandler, Response};
use tidepool_effect::EffectError;

#[path = "staged_world_acceptance_tests.rs"]
mod staged_world_acceptance;

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

    fn prepare(
        &mut self,
        request: ProbeRequest,
        cx: &EffectContext<'_, QuietOutput>,
    ) -> Result<tidepool_effect::dispatch::EffectDispatch, EffectError> {
        if matches!(&request, ProbeRequest::Print(value) if value == "-2147483647") {
            Ok(tidepool_effect::dispatch::EffectDispatch::Unhandled)
        } else {
            self.handle(request, cx)
                .map(tidepool_effect::dispatch::EffectDispatch::Immediate)
        }
    }
}

type ProbeSession = ScaleSession<frunk::HList!(ProbeHandler)>;

struct SemanticSession {
    resident: ProbeSession,
    public: ScopeId,
    public_source: Arc<crate::session::RuntimeLexicalScopeLease>,
    recipe: InputRecipe,
    trace: Arc<CaptureMutex<Vec<i64>>>,
    compiler_counts: Arc<CaptureMutex<Vec<u64>>>,
    history_compiler_census: Option<(u64, Vec<CompilerModeObservation>)>,
    cancel_after_record: Arc<CaptureMutex<Option<Arc<std::sync::atomic::AtomicBool>>>>,
    effects: TestEffectSurface,
    recovery: Option<crate::session::recovery_test_fixture::RecoveryFixture>,
    _root: tempfile::TempDir,
}

fn semantic_recovery_owner() -> RecoveryPublicOwner {
    RecoveryPublicOwner::new(
        &tidepool_repr::ActorPath::parse("root/semantic-history").unwrap(),
        1,
    )
    .unwrap()
}

impl SemanticSession {
    fn new() -> Self {
        Self::with_raw_result(None)
    }

    fn with_raw_result(raw_result: Option<Arc<CaptureMutex<Vec<String>>>>) -> Self {
        Self::with_raw_result_and_recovery(raw_result, false)
    }

    fn durable() -> Self {
        Self::with_raw_result_and_recovery(None, true)
    }

    fn with_raw_result_and_recovery(
        raw_result: Option<Arc<CaptureMutex<Vec<String>>>>,
        durable: bool,
    ) -> Self {
        tidepool_testing::eval_harness::require_extract();
        let root = tidepool_toolchain::artifacts::compiler_scratch_directory().unwrap();
        std::fs::create_dir(root.path().join("support")).unwrap();
        std::fs::write(
            root.path().join("support/SegmentProbeSupport.hs"),
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
        let mut includes = effects.include_paths().to_vec();
        includes.push(root.path().join("support"));
        let recipe = InputRecipe::capture(
            effects.preamble().to_owned(),
            effects.row().to_owned(),
            includes,
        );
        let mut lib = SessionLib::open(
            SessionId(1010),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .unwrap()
        .with_validation_include(recipe.include.clone());
        let recovery = durable.then(|| {
            let recovery =
                crate::session::recovery_test_fixture::RecoveryFixture::create(root.path());
            tidepool_testing::with_settlement(|settlement| {
                recovery.attach_root(&mut lib, settlement)
            })
            .unwrap();
            recovery
        });
        let trace = Arc::new(CaptureMutex::new(Vec::new()));
        let compiler_counts = Arc::new(CaptureMutex::new(Vec::new()));
        let cancel_after_record = Arc::new(CaptureMutex::new(None));
        let mut resident = ResidentSession::unbootstrapped(
            frunk::hlist![ProbeHandler {
                trace: trace.clone(),
                raw_result,
                compiler_counts: compiler_counts.clone(),
                cancel_after_record: cancel_after_record.clone()
            }],
            QuietOutput,
            1024 * 1024,
            Some(lib),
        );
        assert_eq!(
            resident.effect_policy(),
            tidepool_effect::EffectRunPolicy::HandleOrSuspend
        );
        let public_source = resident.mint_notebook_scope_with_lease(None).unwrap();
        let public = public_source.scope();
        if recovery.is_some() {
            assert!(matches!(
                resident
                    .initialize_durable_public_scope(semantic_recovery_owner(), public)
                    .unwrap(),
                PublicManifestCommit::Durable
            ));
        }
        resident
            .set_run_context(SessionRunContext {
                lexical_scope: public,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        bootstrap_world_for(&mut resident, &recipe, &public_source);
        Self {
            resident,
            public,
            public_source,
            recipe,
            trace,
            compiler_counts,
            history_compiler_census: None,
            cancel_after_record,
            effects,
            recovery,
            _root: root,
        }
    }

    fn execute(
        &mut self,
        label: &str,
        source: &str,
        declarations: usize,
    ) -> Result<Duration, ResidentError> {
        self.execute_observed(
            label,
            source,
            CellDeclarationExpectation::Total(declarations),
            SemanticChecks::None,
            &SourceImports::new(),
            None,
            |_| {},
        )
        .map(|(elapsed, _)| elapsed)
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
            self.execute_observed(
                label,
                &history.cell,
                CellDeclarationExpectation::CapturedAt {
                    name: "historyCaptured",
                    line: history.declaration_line,
                },
                SemanticChecks::None,
                &SourceImports::new(),
                None,
                |_| {},
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
        let following = &requests[index..];
        let published = self
            .resident
            .public_visibility_snapshot_in(self.public)
            .unwrap();
        eprintln!(
            "typed-segment-history-compiler-census before_effect={index} total={} post_effect={following:?} published_generation={}",
            requests.len(),
            published.declaration_tip.0,
        );
        assert!(
            following.len() <= 1,
            "only the accepted public declaration join may follow the last effect: {following:?}"
        );
        if let [join] = following {
            assert!(
                join.is_publication_join(published.declaration_tip.0),
                "post-effect work must certify the exact published interface: {join:?}"
            );
        }
    }
}

#[derive(Clone, Copy)]
enum SemanticChecks {
    None,
    Shape(super::SegmentWorkShape),
    NativeEntries,
    ItemOrder { future_item: usize },
}

impl SemanticSession {
    fn execute_observed(
        &mut self,
        label: &str,
        source: &str,
        declarations: CellDeclarationExpectation,
        checks: SemanticChecks,
        imports: &SourceImports,
        preamble: Option<&str>,
        observe: impl FnOnce(&tidepool_toolchain::checked_cell::CellProgram),
    ) -> Result<(Duration, Vec<Arc<PreparedProgram>>), ResidentError> {
        execute_semantic_cell(
            &mut self.resident,
            &self.public_source,
            &self.recipe,
            self.recovery.as_ref().map(|_| semantic_recovery_owner()),
            label,
            source,
            declarations,
            checks,
            imports,
            preamble,
            observe,
        )
    }
}

fn semantic_plan_error(error: crate::session::WorldInspectionError) -> ResidentError {
    match error {
        crate::session::WorldInspectionError::Runtime(crate::RuntimeError::Compile(error)) => {
            crate::session::SessionError::Compile(error).into()
        }
        crate::session::WorldInspectionError::Runtime(error) => ResidentError::Run(error),
        other => panic!("semantic fixture World observation failed: {other:?}"),
    }
}

fn execute_semantic_cell(
    resident: &mut ProbeSession,
    public_source: &Arc<crate::session::RuntimeLexicalScopeLease>,
    recipe: &InputRecipe,
    durable_owner: Option<RecoveryPublicOwner>,
    label: &str,
    source: &str,
    declarations: CellDeclarationExpectation,
    checks: SemanticChecks,
    imports: &SourceImports,
    preamble: Option<&str>,
    observe: impl FnOnce(&tidepool_toolchain::checked_cell::CellProgram),
) -> Result<(Duration, Vec<Arc<PreparedProgram>>), ResidentError> {
    let started = Instant::now();
    let public = public_source.scope();
    let original = public_source.original_source().clone();
    let previous_context = resident.run_context();
    let execution = Arc::new(source_action_with_original(
        resident,
        original.clone(),
        |resident, _| match &durable_owner {
            Some(owner) => resident.begin_durable_private_execution(owner, public),
            None => resident.begin_private_execution(public),
        },
    )?);
    let result = (|| {
        let mut specification = (*fixture_cell_recipe(recipe, source, imports)).clone();
        if let Some(preamble) = preamble {
            specification.template =
                resident_cell_check_template(preamble, &recipe.row, &imports.template_text());
            specification.turn_templates =
                resident_workbench_templates(preamble, &recipe.row, &imports.template_text())
                    .into_iter()
                    .map(|template| (template.kind.wire_name().to_owned(), template.source))
                    .collect();
        }
        let admission =
            plan_fixture_specification_for(resident, Arc::new(specification), execution.clone())
                .map_err(semantic_plan_error)?;
        let plan = admission
            .world_reservation()
            .expect("ordinary semantic cell has a real World reservation")
            .plan();
        use tidepool_toolchain::cell_plan::ParsedCellPlanKind;
        match declarations {
            CellDeclarationExpectation::Total(expected) => assert_eq!(
                plan.items()
                    .iter()
                    .filter(|item| matches!(
                        item.kind(),
                        ParsedCellPlanKind::Prologue | ParsedCellPlanKind::Declaration
                    ))
                    .count(),
                expected
            ),
            CellDeclarationExpectation::CapturedAt { name, line } => {
                let declarations = plan
                    .items()
                    .iter()
                    .filter(|item| item.kind() == ParsedCellPlanKind::Declaration)
                    .collect::<Vec<_>>();
                let [declaration] = declarations.as_slice() else {
                    panic!("one authored declaration group: {declarations:?}")
                };
                assert_eq!(declaration.binders(), [name]);
                assert_eq!(declaration.span().start_line(), line);
                assert_eq!(declaration.span().end_line(), line + 1);
                assert_eq!(declaration.source_ordinals().len(), 2);
                let prologues = plan
                    .items()
                    .iter()
                    .filter(|item| item.kind() == ParsedCellPlanKind::Prologue)
                    .collect::<Vec<_>>();
                assert!(prologues.len() <= 1);
                for prologue in prologues {
                    assert!(prologue.index() < declaration.index());
                    assert_eq!(prologue.span().start_line(), 1);
                    assert!(prologue.source().is_empty() && prologue.binders().is_empty());
                }
            }
        }
        let settled = original
            .with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, report| prepare_cell_program_admitted(owner, admission, report),
            )
            .map_err(|error| ResidentError::Run(error.into()))?;
        let (checked, program) = finish_cell_program_admitted(settled)
            .map_err(|failure| crate::session::SessionError::Compile(failure.error))?;
        observe(&program);
        if let SemanticChecks::Shape(shape) = checks {
            assert_segment_shape(&program, shape);
        }
        if matches!(checks, SemanticChecks::NativeEntries) {
            assert_checked_native_entries(resident, public, &checked, &program);
        }
        let native_targets = program
            .items()
            .iter()
            .filter_map(|item| item.native().map(|native| native.target_owned()))
            .collect();
        let prefix = resident
            .begin_cell_program(program)?
            .expect("nonempty semantic cell has ordered items");
        let future_item = match checks {
            SemanticChecks::NativeEntries => Some(1),
            SemanticChecks::ItemOrder { future_item } => Some(future_item),
            _ => None,
        };
        if let Some(future_item) = future_item {
            assert!(future_item > 0);
            let before = resident.public_visibility_snapshot_in(public).unwrap();
            let before_private = resident
                .public_visibility_snapshot_in(execution.private_scope())
                .unwrap();
            let before_prefix = prefix.snapshot();
            let before_requests = tidepool_extract_cmd::extract_spawn_count();
            assert!(matches!(
                source_action_with_original(resident, original.clone(), |resident, _| {
                    resident.admit_checked_item(
                        prefix.clone(),
                        checked.checked_item(future_item).unwrap(),
                    )
                }),
                Err(crate::session::SessionError::StaleStagedDeclaration)
            ));
            assert_eq!(
                resident.public_visibility_snapshot_in(public).unwrap(),
                before
            );
            assert_eq!(
                resident
                    .public_visibility_snapshot_in(execution.private_scope())
                    .unwrap(),
                before_private
            );
            assert!(Arc::ptr_eq(&before_prefix, &prefix.snapshot()));
            assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);
            assert_eq!(tidepool_extract_cmd::extract_spawn_count(), before_requests);
        }
        let before_effects = tidepool_extract_cmd::extract_spawn_count();
        resident.set_run_context(SessionRunContext {
            lexical_scope: execution.private_scope(),
            ..previous_context
        })?;
        for index in 0..checked.items.len() {
            let item = checked.checked_item(index).unwrap();
            let reservation =
                source_action_with_original(resident, original.clone(), |resident, _| {
                    resident.admit_checked_item(prefix.clone(), item.clone())
                })?;
            if item.kind() == CheckedItemKind::Declaration {
                source_action_with_original(resident, original.clone(), |resident, _| {
                    resident.adopt_checked_declaration(reservation)
                })?;
            } else {
                let TurnResult::Bind {
                    bound, compiled, ..
                } = consume_cell_program_item(reservation.clone()).unwrap()
                else {
                    panic!("checked semantic item must return its binding recipe")
                };
                if matches!(checks, SemanticChecks::NativeEntries) && index == 1 {
                    assert_checked_capture_refusals(&compiled, &reservation);
                }
                let outcome =
                    source_action_with_original(resident, original.clone(), |resident, report| {
                        if item.kind() != CheckedItemKind::Bind {
                            resident.run_observation_with_sites(
                                compiled.code(),
                                &bound[0],
                                reservation.generation(),
                                false,
                                report,
                            )
                        } else if bound.is_empty() {
                            resident.run_with_sites(label, compiled.code(), report)
                        } else if bound.len() == 1 {
                            resident.run_bind_with_sites(
                                &bound[0].name,
                                compiled.code(),
                                &bound[0],
                                reservation.generation(),
                                report,
                            )
                        } else {
                            resident.run_projected_bind_with_sites(
                                label,
                                compiled.code(),
                                &bound,
                                reservation.generation(),
                                report,
                            )
                        }
                    })?;
                if item.kind() == CheckedItemKind::Bind && bound.len() > 1 {
                    assert!(
                        matches!(
                            outcome,
                            crate::session::ResidentOutcome::BindingsCommitted { .. }
                        ),
                        "projected semantic bind must publish all original binders: {outcome:?}"
                    );
                } else {
                    assert!(matches!(outcome, crate::session::ResidentOutcome::Completed { .. }),
                        "semantic native item must complete after dispatching its real effects: {outcome:?}");
                }
            }
            assert_eq!(prefix.snapshot().compiler_prefix().next_item(), index + 1);
            assert_eq!(
                tidepool_extract_cmd::extract_spawn_count(),
                before_effects,
                "native execution consumes the complete admitted program without compiler requests"
            );
        }
        let intent = resident.freeze_private_execution(&execution)?;
        source_action_with_original(resident, original.clone(), |resident, report| {
            let publication = match &durable_owner {
                Some(owner) => resident.restage_execution_publication(owner.clone(), intent)?,
                None => resident.restage_ephemeral_execution_publication(intent)?,
            };
            let ticket = match publication {
                ExecutionPublication::Bindings(base) => base.stage()?,
                ExecutionPublication::Declarations(base) => {
                    let reserved = base.reserved_generation();
                    let span = tracing::info_span!(
                        "publication_declaration_certification",
                        reserved_generation = reserved.0
                    );
                    let _entered = span.enter();
                    let CertifiedDeclarationPublication::Accepted(accepted) =
                        base.certify(report)?
                    else {
                        panic!("actual World declaration publication was refused")
                    };
                    accepted.stage()?
                }
            };
            let committed = resident
                .attempt_public_manifest(ticket, &PublicationDecision::new())
                .map(crate::session::settled_manifest_attempt)?;
            assert!(matches!(
                (durable_owner.as_ref(), committed),
                (Some(_), PublicManifestCommit::Durable) | (None, PublicManifestCommit::Ephemeral)
            ));
            Ok::<_, crate::session::SessionError>(())
        })?;
        Ok((started.elapsed(), native_targets))
    })();
    resident.set_run_context(previous_context).unwrap();
    resident.retire_scope(execution.private_scope());
    result
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
    use super::super::tests::TestEnvGuard;

    let _timing = TestEnvGuard::set("TIDEPOOL_TIMING", "1");
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
    session
        .execute_observed(
            "signed_template_helpers",
            include_str!("fixtures/typed-segment-template-helper-use.hs"),
            CellDeclarationExpectation::Total(0),
            SemanticChecks::Shape(super::SegmentWorkShape::IndependentActions(7)),
            &SourceImports::new(),
            Some(&preamble),
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
            execute_semantic_cell(
                resident, &session.public_source, &session.recipe, None,
                "cancel_after_effect",
                "cancelledValue <- segmentRecord 1 >> pure (2 :: Int); segmentRecord cancelledValue",
                CellDeclarationExpectation::Total(0), SemanticChecks::None,
                &SourceImports::new(), None, |_| {},
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
        session
            .execute_observed(
                &format!("one_segment_{count}"),
                &source,
                CellDeclarationExpectation::Total(0),
                SemanticChecks::Shape(super::SegmentWorkShape::CaptureChain(count)),
                &SourceImports::new(),
                None,
                |_| {},
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

    // This Original captures a value published by a previous World. Its later
    // consumer names only the Original, so loading it must retain the value's
    // interface transitively across the declaration preparation phase.
    let before_capture = Arc::clone(&session.trace);
    session
        .execute_observed(
            "previous_world_value_through_original",
            include_str!("fixtures/typed-segment-previous-world-original.hs"),
            CellDeclarationExpectation::Total(1),
            SemanticChecks::None,
            &SourceImports::new(),
            None,
            |program| {
                assert_eq!(
                    before_capture.lock().as_slice(),
                    &[23, 23],
                    "preparing the new Original cannot execute its later consumer"
                );
                assert_eq!(program.items().len(), 3);
                assert!(program.items()[1]
                    .checked_item()
                    .planned_declaration()
                    .is_some());
                let unrelated = program.items()[0]
                    .native()
                    .expect("the unrelated binding is a real preceding staged value");
                let later = program.items()[2]
                    .native()
                    .expect("the later stage consumes the new Original");
                assert!(
                    later.required_native_imports().all(|(symbol, generation)| {
                        symbol.occurrence != "unrelatedBeforeOriginal"
                            || generation != unrelated.generation()
                    }),
                    "the new Original consumer must not demand the unrelated staged value"
                );
            },
        )
        .unwrap();
    assert_eq!(session.observed(), [23, 23, 23]);
    session
        .execute(
            "previous_world_original_fresh_consumer",
            "segmentRecord previousWorldCapture",
            0,
        )
        .unwrap();
    assert_eq!(session.observed(), [23, 23, 23, 23]);
    session.assert_no_compiler_since_last_effect();
}

#[test]
fn checked_resume_uses_the_parked_owner_and_refuses_revocation_before_effects() {
    use crate::session::{
        ResidentHole, ResidentResumeError, SessionError, SessionTransitionReason,
    };
    #[derive(Clone, Copy)]
    enum Scenario {
        Complete,
        CompleteObservation,
        Abort,
        FenceBeforeCommit,
        RevokedScope,
        RevokedOwner,
    }
    for scenario in [
        Scenario::Complete,
        Scenario::CompleteObservation,
        Scenario::Abort,
        Scenario::FenceBeforeCommit,
        Scenario::RevokedScope,
        Scenario::RevokedOwner,
    ] {
        let mut session = SemanticSession::new();
        let original = session.public_source.original_source().clone();
        let execution = Arc::new(
            source_action_with_original(&mut session.resident, original.clone(), |resident, _| {
                resident.begin_private_execution(session.public)
            })
            .unwrap(),
        );
        let specification = fixture_cell_recipe(
            &session.recipe,
            include_str!("fixtures/typed-segment-checked-resume.hs"),
            &SourceImports::new(),
        );
        let admission =
            plan_fixture_specification_for(&mut session.resident, specification, execution.clone())
                .unwrap();
        let prepared = original
            .with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                crate::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, report| prepare_cell_program_admitted(owner, admission, report),
            )
            .unwrap();
        let (checked, program) = finish_cell_program_admitted(prepared).unwrap();
        let prefix = session
            .resident
            .begin_cell_program(program)
            .unwrap()
            .unwrap();
        let reservation =
            source_action_with_original(&mut session.resident, original.clone(), |resident, _| {
                resident.admit_checked_item(prefix.clone(), checked.checked_item(0).unwrap())
            })
            .unwrap();
        let TurnResult::Bind {
            bound, compiled, ..
        } = consume_cell_program_item(reservation.clone()).unwrap()
        else {
            panic!("checked fixture is one authored binding");
        };
        session
            .resident
            .set_run_context(SessionRunContext {
                lexical_scope: execution.private_scope(),
                ..SessionRunContext::ROOT
            })
            .unwrap();
        let parked = source_action_with_original(
            &mut session.resident,
            original.clone(),
            |resident, report| {
                if matches!(scenario, Scenario::CompleteObservation) {
                    return resident.run_observation_with_sites(
                        compiled.code(),
                        &bound[0],
                        reservation.generation(),
                        true,
                        report,
                    );
                }
                resident.run_bind_with_sites(
                    "parkedValue",
                    compiled.code(),
                    &bound[0],
                    reservation.generation(),
                    report,
                )
            },
        )
        .unwrap();
        let crate::session::ResidentOutcome::Suspended { hole, .. } = parked else {
            panic!("the real Console request must park the checked binding");
        };
        assert_eq!(session.observed(), [1]);
        assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);
        let rewrapped = ResidentHole::plain(hole.cont_id());
        // The saved owner determines lexical scope; changing checkout context
        // cannot replace it with the caller's public lexical environment.
        session
            .resident
            .set_run_context(SessionRunContext {
                lexical_scope: session.public,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        match scenario {
            Scenario::Complete | Scenario::CompleteObservation => {
                let completed = source_action_with_original(
                    &mut session.resident,
                    original,
                    |resident, report| resident.resume_classified(rewrapped.clone(), (), report),
                )
                .unwrap();
                assert!(matches!(
                    completed,
                    crate::session::ResidentOutcome::Completed { .. }
                ));
                assert_eq!(session.observed(), [1, 2]);
                assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 1);
                assert!(session
                    .resident
                    .current_binding_in(execution.private_scope(), "parkedValue")
                    .is_some());
                let repeated = tidepool_testing::with_settlement(|report| {
                    session.resident.resume_classified(rewrapped, (), report)
                });
                assert!(matches!(
                    repeated,
                    Err(ResidentResumeError::Rejected(
                        ResidentError::WrongContinuation { .. }
                    ))
                ));
                assert_eq!(session.observed(), [1, 2]);
            }
            Scenario::Abort => {
                let aborted = session
                    .resident
                    .abort(hole.cont_id(), "retire checked owner".into());
                assert!(matches!(aborted, Err(ResidentError::Run(_))));
                assert_eq!(session.observed(), [1]);
                assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);
                assert!(session
                    .resident
                    .freeze_private_execution(&execution)
                    .is_err());
            }
            Scenario::FenceBeforeCommit => {
                let fenced = Arc::new(std::sync::Mutex::new(None));
                let observed_fence = fenced.clone();
                let gate = original.source_admission().clone();
                let pending_prefix = prefix.clone();
                let observer = Arc::new(move |event| {
                    if matches!(
                        event,
                        crate::session::resident::ResidentContinuationEvent::Retired(_)
                    ) {
                        assert_eq!(pending_prefix.snapshot().compiler_prefix().next_item(), 0);
                        let gate = gate.clone();
                        let fence = std::thread::spawn(move || gate.fence())
                            .join()
                            .unwrap()
                            .unwrap();
                        *observed_fence.lock().unwrap() = Some(fence);
                    }
                });
                let refused = session
                    .resident
                    .with_continuation_observer(observer, |resident| {
                        source_action_with_original(resident, original, |resident, report| {
                            resident.resume_classified(rewrapped, (), report)
                        })
                    });
                assert!(
                    fenced.lock().unwrap().is_some(),
                    "the actual consumed continuation reached capture"
                );
                assert!(matches!(
                    refused,
                    Err(ResidentResumeError::Consumed(ResidentError::Session(
                        SessionError::SourceAdmission(
                            crate::session::SourceAdmissionError::Closed { .. }
                        )
                    )))
                ));
                assert_eq!(
                    session.observed(),
                    [1, 2],
                    "completed effects are not replayed or rolled back"
                );
                assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);
                assert!(session
                    .resident
                    .current_binding_in(execution.private_scope(), "parkedValue")
                    .is_none());
                assert!(session
                    .resident
                    .retained_checked_value_artifact(tidepool_repr::SessionModule::val(
                        reservation.generation()
                    ))
                    .is_none());
                assert!(session
                    .resident
                    .freeze_private_execution(&execution)
                    .is_err());
            }
            Scenario::RevokedScope | Scenario::RevokedOwner => {
                let expected = if matches!(scenario, Scenario::RevokedScope) {
                    session.resident.retire_scope(execution.private_scope());
                    SessionTransitionReason::ScopeRevoked
                } else {
                    let next = execution.owner.epoch().checked_add(1).unwrap();
                    execution
                        .owner
                        .with_source_transition(|transition| transition.advance_epoch(next));
                    SessionTransitionReason::OwnerChanged
                };
                let refused = tidepool_testing::with_settlement(|report| {
                    session.resident.resume_classified(rewrapped, (), report)
                });
                assert!(
                    matches!(refused, Err(ResidentResumeError::Rejected(ResidentError::Session(SessionError::TransitionRefused(reason)))) if reason == expected)
                );
                assert_eq!(
                    session.observed(),
                    [1],
                    "revocation must refuse before the second effect"
                );
                assert_eq!(prefix.snapshot().compiler_prefix().next_item(), 0);
                // Refusal leaves the continuation parked; explicit retirement
                // must release its captured owner without executing it.
                assert!(session
                    .resident
                    .abort(hole.cont_id(), "retire refused owner".into())
                    .is_err());
            }
        }
        drop(hole);
        drop(reservation);
        drop(compiled);
        drop(prefix);
        drop(checked);
        drop(execution);
        assert_eq!(session.resident.outstanding_custody(), 0);
    }
}

#[test]
fn historical_source_captures_bind_the_original_value_after_shadowing() {
    use tidepool_toolchain::compiler_world::DependencyTerminalOrigin;
    for (label, source, expected_origin) in [
        (
            "inline_historical_capture",
            include_str!("fixtures/typed-segment-inline-historical-capture.hs"),
            DependencyTerminalOrigin::SourceDefinition,
        ),
        (
            "noinline_historical_capture",
            include_str!("fixtures/typed-segment-noinline-historical-capture.hs"),
            DependencyTerminalOrigin::NativeOriginal,
        ),
    ] {
        let mut session = SemanticSession::new();
        session
            .execute(label, "historyValue0 <- pure (23 :: Int)", 0)
            .unwrap();
        let original = session
            .resident
            .current_binding_in(session.public, "historyValue0")
            .unwrap();
        let original_interface = session
            .resident
            .retained_checked_value_artifact(original.1)
            .unwrap()
            .certified_interface()
            .artifact_id();
        session.execute(label, source, 1).unwrap();
        session
            .execute(label, "historyValue0 <- pure (91 :: Int)", 0)
            .unwrap();
        let replacement = session
            .resident
            .current_binding_in(session.public, "historyValue0")
            .unwrap();
        assert_ne!(original.0, replacement.0);
        assert_ne!(original.1, replacement.1);
        let before_effects = Arc::clone(&session.trace);
        session
            .execute_observed(
                label,
                "segmentRecord historicalCapture",
                CellDeclarationExpectation::Total(0),
                SemanticChecks::None,
                &SourceImports::new(),
                None,
                |program| {
                    assert!(before_effects.lock().is_empty());
                    let terminals = program
                        .items()
                        .iter()
                        .flat_map(|item| {
                            item.checked_item()
                                .world_stage_dependencies()
                                .map(|owner| owner.terminals().collect::<Vec<_>>())
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>();
                    assert!(terminals.iter().any(|terminal| {
                        terminal.origin() == expected_origin
                            && terminal.identity().module == original.1.module_name()
                            && terminal.generation() == original.1.gen.0
                            && terminal.binding().artifact == original_interface
                    }), "the actual selected {expected_origin:?} occurrence must retain the historical value");
                    assert!(terminals.iter().all(|terminal| {
                        terminal.identity().module != replacement.1.module_name()
                    }), "the new visible spelling cannot replace the captured dependency");
                },
            )
            .unwrap();
        assert_eq!(session.observed(), [23]);
        session.assert_no_compiler_since_last_effect();
        session
            .execute(label, "segmentRecord historyValue0", 0)
            .unwrap();
        assert_eq!(session.observed(), [23, 91]);
        let before_retirement = session.resident.residency().unwrap().persistent_roots;
        let retired = session.resident.retire_scope(session.public);
        assert!(retired.roots_released > 0);
        assert_eq!(
            session.resident.residency().unwrap().persistent_roots,
            before_retirement - retired.roots_released,
            "retiring the runtime owner releases its actual prepared binding roots"
        );
    }
}

#[derive(Clone, Default)]
struct SourceQueryObserver(Arc<CaptureMutex<Vec<(String, u64, String)>>>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SourceQueryObserver {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if event.metadata().target() != "tidepool_toolchain::compiler_world::native_demand" {
            return;
        }
        let mut fields = RequestFields::default();
        event.record(&mut fields);
        if fields.0.get("message").map(String::as_str) != Some("source_dependency_query") {
            return;
        }
        self.0.lock().push((
            fields.0["admission"].clone(),
            fields.0["stage"].parse().unwrap(),
            fields.0["binder_occurrence"].clone(),
        ));
    }
}

#[test]
fn same_cell_source_capture_requeries_only_the_referenced_prior_output() {
    use tidepool_toolchain::compiler_world::DependencyTerminalOrigin;
    let mut session = SemanticSession::new();
    session
        .execute("same_cell_source", "historyValue0 <- pure (23 :: Int)", 0)
        .unwrap();
    let original = session
        .resident
        .current_binding_in(session.public, "historyValue0")
        .unwrap();
    let original_interface = session
        .resident
        .retained_checked_value_artifact(original.1)
        .unwrap()
        .certified_interface()
        .artifact_id();
    session
        .execute(
            "same_cell_source",
            include_str!("fixtures/typed-segment-inline-historical-capture.hs"),
            1,
        )
        .unwrap();
    session
        .execute("same_cell_source", "historyValue0 <- pure (91 :: Int)", 0)
        .unwrap();
    let replacement = session
        .resident
        .current_binding_in(session.public, "historyValue0")
        .unwrap();
    assert_ne!(original.0, replacement.0);
    assert_ne!(original.1, replacement.1);

    let observer = SourceQueryObserver::default();
    let before_effects = Arc::clone(&session.trace);
    tracing::subscriber::with_default(
        tracing_subscriber::registry().with(observer.clone()),
        || {
            session
                .execute_observed(
                    "same_cell_source",
                    include_str!("fixtures/typed-segment-same-cell-source-capture.hs"),
                    CellDeclarationExpectation::Total(1),
                    SemanticChecks::None,
                    &SourceImports::new(),
                    None,
                    |program| {
                        assert!(before_effects.lock().is_empty());
                        let terminals = program
                            .items()
                            .iter()
                            .filter(|item| item.native().is_some())
                            .flat_map(|item| {
                                item.checked_item()
                                    .world_stage_dependencies()
                                    .unwrap()
                                    .terminals()
                            })
                            .collect::<Vec<_>>();
                        assert!(
                            terminals.iter().any(|terminal| {
                                terminal.origin() == DependencyTerminalOrigin::SourceDefinition
                                    && terminal.identity().module == original.1.module_name()
                                    && terminal.generation() == original.1.gen.0
                                    && terminal.binding().artifact == original_interface
                            }),
                            "the executable stage must seal the old exact SOURCE occurrence"
                        );
                        assert!(
                            terminals.iter().all(|terminal| {
                                terminal.identity().module != replacement.1.module_name()
                            }),
                            "the later visible value must not replace the inherited SOURCE capture"
                        );
                    },
                )
                .unwrap();
        },
    );
    // NativeDemandResolver is borrowed synchronously by the endpoint response
    // loop. Stage zero proves the subscriber saw this exact resolver admission.
    let queries = observer.0.lock();
    let admissions = queries
        .iter()
        .filter_map(|(admission, stage, occurrence)| {
            (*stage == 0 && occurrence == "historicalCapture").then_some(admission)
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        admissions.len(),
        1,
        "one actual stage-zero SOURCE operation"
    );
    let admission = *admissions.first().unwrap();
    assert!(
        queries.iter().any(|(actual, stage, occurrence)| {
            actual == admission && *stage == 1 && occurrence == "historicalCapture"
        }),
        "the used predecessor output must requery its SOURCE use under the new stage fence"
    );
    drop(queries);
    assert_eq!(session.observed(), [23]);
    session.assert_no_compiler_since_last_effect();

    let observer = SourceQueryObserver::default();
    tracing::subscriber::with_default(
        tracing_subscriber::registry().with(observer.clone()),
        || {
            session
                .execute(
                    "unused_same_cell_source",
                    include_str!("fixtures/typed-segment-unused-same-cell-source-capture.hs"),
                    1,
                )
                .unwrap();
        },
    );
    let queries = observer.0.lock();
    let admissions = queries
        .iter()
        .filter_map(|(admission, stage, occurrence)| {
            (*stage == 0 && occurrence == "historicalCapture").then_some(admission)
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        admissions.len(),
        1,
        "the declaration publication must positively observe its actual SOURCE input"
    );
    let admission = *admissions.first().unwrap();
    assert!(queries.iter().all(|(actual, stage, occurrence)| {
        actual != admission || *stage != 1 || occurrence != "historicalCapture"
    }), "an unused predecessor output must issue no SOURCE query in the executable stage of the same admission");
    assert_eq!(session.observed(), [23, 99]);
    session.assert_no_compiler_since_last_effect();
}

#[test]
fn compiler_issued_binding_survives_capture_and_releases_interface_and_root() {
    let mut session = SemanticSession::new();
    let roots = |resident: &ProbeSession| {
        resident
            .residency()
            .map_or(0, |counts| counts.persistent_roots)
    };
    let mut certificate = None;
    session
        .execute_observed(
            "binding_index_real_producer",
            "indexedValue <- pure (41 :: Int)",
            CellDeclarationExpectation::Total(0),
            SemanticChecks::None,
            &SourceImports::new(),
            None,
            |program| {
                let [item] = program.items() else {
                    panic!("producer admits one native binding");
                };
                certificate = Some(
                    item.native()
                        .unwrap()
                        .value_interface_certificate()
                        .unwrap(),
                );
            },
        )
        .unwrap();
    let certificate = certificate.unwrap();
    let original = session
        .resident
        .current_binding_in(session.public, "indexedValue")
        .unwrap();
    assert_eq!(original.1, certificate.owner());
    assert_eq!(
        session.resident.inject_val_modules(),
        [original.1.module_name()]
    );
    assert_eq!(
        session.resident.current_val_modules_in(session.public),
        [original.1.module_name()]
    );
    assert!(Arc::ptr_eq(
        session
            .resident
            .retained_checked_value_artifact(original.1)
            .unwrap(),
        &certificate,
    ));
    let produced_roots = roots(&session.resident);
    assert!(produced_roots > 0);

    // This retained lexical tip, rather than a new certificate or copied value,
    // owns the original root while its issuer's scope is retired.
    let consumer = session
        .resident
        .mint_detached_scope(session.public)
        .unwrap();
    let retired = session.resident.retire_scope(session.public);
    assert_eq!(retired.bindings_retired, 0);
    assert_eq!(retired.roots_released, 0);
    assert_eq!(roots(&session.resident), produced_roots);
    assert!(session
        .resident
        .current_binding_in(session.public, "indexedValue")
        .is_none());
    assert_eq!(
        session
            .resident
            .current_binding_in(consumer, "indexedValue"),
        Some(original.clone())
    );
    assert!(Arc::ptr_eq(
        session
            .resident
            .retained_checked_value_artifact(original.1)
            .unwrap(),
        &certificate,
    ));
    let certificate_lifetime = Arc::downgrade(&certificate);
    drop(certificate);
    session.public = consumer;
    session.public_source = session
        .resident
        .adopt_initial_notebook_scope(consumer)
        .unwrap();
    session
        .resident
        .set_run_context(SessionRunContext {
            lexical_scope: consumer,
            ..SessionRunContext::ROOT
        })
        .unwrap();
    bootstrap_world_for(
        &mut session.resident,
        &session.recipe,
        &session.public_source,
    );
    session
        .execute(
            "binding_index_real_consumer",
            "segmentRecord (indexedValue + 1)",
            0,
        )
        .unwrap();
    assert_eq!(session.observed(), [42]);
    // Cell capsules queue lease release outside checkout. Use the lifecycle
    // owner's checkpoint before testing the remaining captured scope's release.
    assert_eq!(session.resident.outstanding_custody(), 0);
    // The consumer's final unit observation is also a checked native binding.
    let published = session
        .resident
        .public_visibility_snapshot_in(consumer)
        .unwrap();
    assert_eq!(published.bindings.len(), 2);
    assert!(published.bindings.iter().any(|(_, id)| *id == original.0));
    // Native installs also own package/CAF roots. Retirement must release the
    // binding root from this immediate ledger, rather than return to a cold
    // machine's total before those native artifacts were installed.
    let before_retirement = roots(&session.resident);
    assert!(before_retirement >= produced_roots);
    assert_eq!(
        session
            .resident
            .retained_checked_value_artifact(original.1)
            .map(Arc::as_ptr),
        Some(certificate_lifetime.as_ptr()),
        "the executed consumer retains the original issued certificate until retirement"
    );

    let retired = session.resident.retire_scope(consumer);
    assert_eq!(retired.bindings_retired, 2);
    assert_eq!(retired.roots_released, 2);
    assert_eq!(roots(&session.resident), before_retirement - 2);
    assert!(session
        .resident
        .retained_checked_value_artifact(original.1)
        .is_none());
    assert!(
        certificate_lifetime.upgrade().is_none(),
        "last binding releases the issued certificate"
    );
    assert!(session.resident.inject_val_modules().is_empty());
    assert!(session.resident.current_val_modules_in(consumer).is_empty());
    assert!(session
        .resident
        .current_binding_in(consumer, "indexedValue")
        .is_none());
    assert_eq!(
        session.resident.retire_scope(consumer),
        crate::session::ScopeRetirement::default()
    );
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
        let table = native.table();
        let turn: ciborium::value::Value =
            ciborium::de::from_reader(item.native_turn_bytes().unwrap()).unwrap();
        let fields = turn.as_array().unwrap()[1].as_array().unwrap();
        let sites = tidepool_toolchain::artifacts::decode_turn_yield_sites(&fields[3]).unwrap();
        native
            .original_execution_context(&native.target_owned(), table, &sites)
            .unwrap()
    }

    let mut session = SemanticSession::new();
    let mut cold = None;
    session
        .execute_observed(
            "warm_native_cold_baseline",
            "baselinePlannedValue <- pure (Carrier.carrierBaseline 2)",
            CellDeclarationExpectation::Total(0),
            SemanticChecks::None,
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
    let initially_selected_carrier = before
        .artifact_view()
        .selected_native_groups()
        .into_iter()
        .filter(|key| key.artifact == descriptor.id)
        .collect::<BTreeSet<_>>();
    assert!(initially_selected_carrier.contains(&baseline));
    assert!(!initially_selected_carrier.contains(&increment));
    assert!(!initially_selected_carrier.contains(&double));
    assert!(!initially_selected_carrier.contains(&negate));
    // Per-target imports are distinct from retained heap values and installed code custody.
    let expected_warm_carrier = BTreeSet::from([increment, double]);
    let mut selected_carrier = BTreeSet::new();
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
    session
        .execute_observed(
            "warm_native_carrier_demand",
            include_str!("fixtures/typed-segment-warm-carrier.hs"),
            CellDeclarationExpectation::Total(0),
            SemanticChecks::None,
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
                    let selected = products.artifact_view().selected_native_groups();
                    assert!(selected.contains(&NativeGroupKey {
                        artifact,
                        original_ordinal
                    }));
                    let carrier_groups = selected
                        .into_iter()
                        .filter(|key| key.artifact == descriptor.id)
                        .collect::<BTreeSet<_>>();
                    let context_carrier = context
                        .artifact_view()
                        .selected_native_groups()
                        .into_iter()
                        .filter(|key| key.artifact == descriptor.id)
                        .collect::<BTreeSet<_>>();
                    assert!(carrier_groups.is_subset(&context_carrier));
                    assert!(carrier_groups.is_subset(&expected_warm_carrier));
                    assert!(!context_carrier.contains(&negate));
                    if native.item().binders() == ["warmCarrierResult"] {
                        assert_eq!(carrier_groups, expected_warm_carrier);
                        assert_eq!(context_carrier, expected_warm_carrier);
                    }
                    selected_carrier.extend(carrier_groups);
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
    assert!(initially_selected_carrier.is_disjoint(&result_selected_carrier));
    assert_eq!(result_selected_carrier, expected_warm_carrier);
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
    session
        .execute_observed(
            "native_entry_refusal",
            source,
            CellDeclarationExpectation::CapturedAt {
                name: "boundaryCapture",
                line,
            },
            SemanticChecks::NativeEntries,
            &SourceImports::new(),
            None,
            |_| {},
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

// The finite parser/worker contract has its own artifact-layout and stderr
// controls. These tests do not establish ordinary World-path acceptance.
mod world_component_controls {
    use super::*;

    struct WorldControlSession {
        resident: ProbeSession,
        public: ScopeId,
        _public_source: Arc<crate::session::RuntimeLexicalScopeLease>,
        trace: Arc<CaptureMutex<Vec<i64>>>,
        effects: TestEffectSurface,
        images: Arc<ImageRegistry>,
        _root: tempfile::TempDir,
    }

    impl WorldControlSession {
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
            let public_source = persistent.adopt_initial_notebook_scope(public).unwrap();
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
                _public_source: public_source,
                trace,
                effects,
                images,
                _root: root,
            }
        }

        fn execute(
            &mut self,
            label: &str,
            source: &str,
            declarations: CellDeclarationExpectation,
            checks: AuthorityChecks<'_>,
            imports: &SourceImports,
            observe: impl FnOnce(&tidepool_toolchain::checked_cell::CellProgram),
        ) {
            try_execute_cell_with_template_imports_expectation_observed(
                &mut self.resident,
                self.public,
                &self.effects,
                &self.images,
                (0, 0),
                label,
                source,
                declarations,
                &ScalePublication::Ephemeral,
                checks,
                imports,
                None,
                observe,
            )
            .unwrap();
        }
    }

    #[test]
    fn world_checked_items_refuse_cross_item_mutations_before_effects() {
        let mut session = WorldControlSession::with_raw_result(None);
        session.execute(
            "world_receipt_baseline",
            "baselinePlannedValue <- pure (2 :: Int)",
            CellDeclarationExpectation::Total(0),
            AuthorityChecks::Configured,
            &SourceImports::new(),
            |_| {},
        );
        let source = include_str!("fixtures/typed-segment-native-entry.hs");
        let line = source
            .lines()
            .position(|line| line == "boundaryCapture :: Int")
            .unwrap()
            + 1;
        session.execute(
            "world_receipt_mutations",
            source,
            CellDeclarationExpectation::CapturedAt {
                name: "boundaryCapture",
                line,
            },
            AuthorityChecks::TypedEntryRefusalBranches(concat!(
                module_path!(),
                "::world_checked_items_refuse_cross_item_mutations_before_effects"
            )),
            &SourceImports::new(),
            |_| {},
        );
        assert_eq!(*session.trace.lock(), [4]);
    }

    #[test]
    fn world_inference_ledgers_record_single_pass_segments() {
        use super::super::super::tests::TestEnvGuard;
        let _daemon = TestEnvGuard::unset("TIDEPOOL_EXTRACT_DAEMON_SOCKET");
        let _timing = TestEnvGuard::set("TIDEPOOL_TIMING", "1");
        for count in [1, 4, 8] {
            let cache = tempfile::tempdir().unwrap();
            let _cache = TestEnvGuard::set("TIDEPOOL_COMPILE_CACHE_DIR", cache.path());
            let mut session = WorldControlSession::with_raw_result(None);
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
            session.execute(
                "world_inference_ledger",
                &source,
                CellDeclarationExpectation::Total(0),
                AuthorityChecks::SegmentWorkCounts(super::super::SegmentWorkShape::CaptureChain(
                    count,
                )),
                &SourceImports::new(),
                |_| {},
            );
            assert_eq!(
                *session.trace.lock(),
                (1..=count as i64).collect::<Vec<_>>()
            );
        }
        let results = Arc::new(CaptureMutex::new(Vec::new()));
        let mut session = WorldControlSession::with_raw_result(Some(results.clone()));
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
            "world_signed_template_ledger",
            include_str!("fixtures/typed-segment-template-helper-use.hs"),
            CellDeclarationExpectation::Total(0),
            &ScalePublication::Ephemeral,
            AuthorityChecks::SegmentWorkCounts(super::super::SegmentWorkShape::IndependentActions(
                7,
            )),
            &SourceImports::new(),
            None,
            &preamble,
            |_| {},
        )
        .unwrap();
        assert_eq!(results.lock().len(), 1);
    }

    #[test]
    fn world_warm_emission_excludes_previously_issued_owners() {
        let mut session = WorldControlSession::with_raw_result(None);
        let imports = SourceImports::from_specs(["qualified SegmentProbeSupport as Carrier"]);
        let mut old_owners = std::collections::BTreeSet::new();
        session.execute(
            "world_emission_baseline",
            "baselinePlannedValue <- pure (Carrier.carrierBaseline 2)",
            CellDeclarationExpectation::Total(0),
            AuthorityChecks::Configured,
            &imports,
            |program| {
                let native = program.items()[0].native().unwrap();
                let turn: ciborium::value::Value =
                    ciborium::de::from_reader(program.items()[0].native_turn_bytes().unwrap())
                        .unwrap();
                let fields = turn.as_array().unwrap()[1].as_array().unwrap();
                let sites =
                    tidepool_toolchain::artifacts::decode_turn_yield_sites(&fields[3]).unwrap();
                let context = native
                    .original_execution_context(&native.target_owned(), native.table(), &sites)
                    .unwrap();
                old_owners.extend(
                    context.recovery_products().iter().map(|product| {
                        (product.owner().unit.clone(), product.owner().module.clone())
                    }),
                );
            },
        );
        assert!(!old_owners.is_empty());
        session.execute(
            "world_warm_emission",
            include_str!("fixtures/typed-segment-warm-carrier.hs"),
            CellDeclarationExpectation::Total(0),
            AuthorityChecks::NativeEmissionOwnersAbsent(&old_owners),
            &imports,
            |_| {},
        );
        assert_eq!(*session.trace.lock(), [7]);
    }
}
