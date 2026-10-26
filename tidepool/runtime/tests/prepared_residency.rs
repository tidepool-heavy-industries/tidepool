//! S4a acceptance: a prepared session's between-turn quiescent point
//! actually drains a major collection, so residency stays bounded across
//! many turns instead of growing one program per turn forever.
//!
//! The collection itself is amortized (`PreparedEngine::quiesce_and_collect`,
//! `tidepool/runtime/src/session/prepared.rs`): it only actually runs a
//! major collection every `MAJOR_COLLECTION_INSTALL_INTERVAL` installed
//! programs (or sooner on enough root-block byte growth), not after every
//! turn. `N` below mirrors that constant -- it is not reachable from here
//! (private to `prepared.rs`, and `ResidentSession` exposes only
//! `residency()`/`old_bytes()`, no install-count accessor), so this file
//! treats it as a fixture constant instead. Each expression/bind turn below
//! installs exactly one program (confirmed by the pre-amortization version
//! of this test), so a major collection lands deterministically every `N`th
//! turn as long as byte growth never trips the earlier threshold first --
//! true for these tiny arithmetic turns. Counters are therefore only
//! expected to be flat turn-over-turn *at* those collection boundaries; in
//! between, `programs` (and the other counts) may grow up to the
//! amortized bound before the next collection folds them back down.
//!
//! Needs a resolvable `$TIDEPOOL_EXTRACT` and its Haskell worker (as
//! `prepared_turn.rs` does); no Core-route counterpart -- `residency()` is
//! `None` on Core, so there is nothing to assert there.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tidepool_repr::Generation;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, BoundBinder, CompiledTurn, ModuleEnv, ResidentOutcome,
    ResidentSession, RuntimeLexicalScopeLease, SessionLib, TurnRequest, TurnResult, TurnTemplate,
};
use tidepool_testing::effect_surface::TestEffectSurface;
use tidepool_testing::eval_harness;

/// The minimal parts of `prepared_turn.rs`'s own `Notebook` this file needs:
/// one resident session, its compile plumbing, and expression/bind turns.
struct Notebook {
    session: ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    source: Arc<RuntimeLexicalScopeLease>,
    preamble: String,
    effect_stack: String,
    include: Vec<PathBuf>,
    root: tempfile::TempDir,
    injected: Vec<String>,
    generation: u64,
}

fn source_action<T>(
    session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    source: &RuntimeLexicalScopeLease,
    action: impl FnOnce(
        &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
    ) -> T,
) -> T {
    let settled = session
        .with_source_compiler_transaction(
            source.original_source().clone(),
            tidepool_toolchain::artifacts::CompileWorkload::Foreground,
            tidepool_runtime::CompilerTransactionCancellation::new(),
            None,
            |_| {},
            action,
        )
        .expect("admit the fixture's live source owner");
    assert!(
        matches!(
            settled.close(),
            tidepool_runtime::CompilerTransactionClose::Clean
                | tidepool_runtime::CompilerTransactionClose::NotStarted
        ),
        "source transaction did not retire cleanly: {:?}",
        settled.close()
    );
    settled.into_outcome().action
}

struct CheckedExecution {
    execution: Arc<tidepool_runtime::session::PrivateExecutionAdmission>,
    source: Arc<RuntimeLexicalScopeLease>,
    preamble: String,
    row: String,
}

impl CheckedExecution {
    fn start(
        session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        includes: &[PathBuf],
        preamble: &str,
        row: &str,
    ) -> Self {
        use tidepool_runtime::session::SessionRunContext;
        let source = session.mint_notebook_scope_with_lease(None).unwrap();
        let public = source.scope();
        session
            .set_run_context(SessionRunContext {
                lexical_scope: public,
                ..SessionRunContext::ROOT
            })
            .unwrap();
        super::compiler_test_support::install_world(session, &source, includes, preamble, row);
        let execution = Arc::new(
            source_action(session, &source, |session, _| {
                session.begin_ephemeral_private_execution(public)
            })
            .unwrap(),
        );
        session
            .set_run_context(SessionRunContext {
                lexical_scope: execution.private_scope(),
                ..SessionRunContext::ROOT
            })
            .unwrap();
        Self {
            execution,
            source,
            preamble: preamble.to_owned(),
            row: row.to_owned(),
        }
    }

    fn specification(
        &self,
        text: &str,
        extra_imports: &str,
    ) -> Arc<tidepool_extract_cmd::WorldCellRecipe> {
        use tidepool_runtime::session::turn::TemplateSelector;
        use tidepool_runtime::session::{resident_cell_check_template, SourceImports};
        let imports = SourceImports::from_specs(extra_imports.lines()).template_text();
        let templates = resident_workbench_templates(&self.preamble, &self.row, &imports);
        Arc::new(tidepool_extract_cmd::WorldCellRecipe {
            source: text.to_owned(),
            template: resident_cell_check_template(&self.preamble, &self.row, &imports),
            turn_templates: templates
                .iter()
                .map(|template| {
                    let selector = match template.kind {
                        TemplateSelector::Decl => "decl",
                        TemplateSelector::Bind => "bind",
                        TemplateSelector::BindDiscard => "binddiscard",
                        TemplateSelector::Expr => "expr",
                    };
                    (selector.to_owned(), template.source.clone())
                })
                .collect(),
        })
    }

    fn compile(
        &self,
        session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        text: &str,
        extra_imports: &str,
    ) -> (
        BoundBinder,
        CompiledTurn,
        Arc<tidepool_runtime::session::RuntimeCheckedItemAdmission>,
    ) {
        let admission = super::compiler_test_support::plan_checked_cell(
            session,
            &self.source,
            self.execution.clone(),
            self.specification(text, extra_imports),
        );
        self.consume(session, admission, text)
    }

    fn consume(
        &self,
        session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        admission: Arc<tidepool_runtime::session::RuntimeCellAdmission>,
        label: &str,
    ) -> (
        BoundBinder,
        CompiledTurn,
        Arc<tidepool_runtime::session::RuntimeCheckedItemAdmission>,
    ) {
        use tidepool_runtime::session::turn::{
            consume_cell_program_item, finish_cell_program_admitted, prepare_cell_program_admitted,
        };
        let settled = self
            .source
            .with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, report| prepare_cell_program_admitted(owner, admission, report),
            )
            .unwrap();
        let (checked, program) = finish_cell_program_admitted(settled)
            .unwrap_or_else(|error| panic!("checked fixture {label:?}: {error:?}"));
        assert_eq!(checked.items.len(), 1, "one checked fixture item");
        let reservation = source_action(session, &self.source, |session, _| {
            let prefix = session.begin_cell_program(program).unwrap().unwrap();
            session.admit_checked_item(prefix, checked.checked_item(0).unwrap())
        })
        .unwrap();
        let TurnResult::Bind {
            bound, compiled, ..
        } = consume_cell_program_item(reservation.clone()).unwrap()
        else {
            panic!("checked fixture produces its issued binding");
        };
        assert_eq!(bound.len(), 1);
        (bound.into_iter().next().unwrap(), compiled, reservation)
    }

    fn host_carrier(
        &self,
        session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        name: &str,
        expected: tidepool_runtime::session::HostBindingType,
        anchor: &str,
        imports: &str,
    ) -> tidepool_runtime::session::HostCarrier {
        use tidepool_runtime::session::{
            turn::{finish_world_cell_plan_admitted, prepare_world_cell_plan_admitted},
            HostCarrier,
        };
        let digest = [19; 32];
        let preparation = source_action(session, &self.source, |session, _| {
            session.prepare_private_host_carrier(self.execution.clone(), digest)
        })
        .unwrap();
        if let Some(prototype) = session.host_binding_prototype(expected, digest) {
            let before_requests = tidepool_extract_cmd::extract_spawn_count();
            let before_interfaces =
                tidepool_toolchain::artifacts::host_binding_interface_request_count();
            let admission = source_action(session, &self.source, |session, _| {
                session.admit_private_host_binding_interface(
                    preparation,
                    name.to_owned(),
                    prototype.clone(),
                )
            })
            .unwrap();
            let interface = source_action(session, &self.source, |_, report| {
                tidepool_toolchain::artifacts::issue_host_binding_interface(
                    prototype.compiler().clone(),
                    admission.digest(),
                    admission.generation().0,
                    admission.binding(),
                    prototype.include_paths(),
                    report,
                )
            })
            .unwrap();
            assert!(
                Arc::ptr_eq(
                    &prototype,
                    &session.host_binding_prototype(expected, digest).unwrap()
                ),
                "reuse the original checked constructor prototype"
            );
            assert_eq!(
                tidepool_extract_cmd::extract_spawn_count(),
                before_requests + 1,
                "prototype reuse issues only its new binding interface"
            );
            assert_eq!(
                tidepool_toolchain::artifacts::host_binding_interface_request_count(),
                before_interfaces + 1
            );
            return HostCarrier::from_interface(admission, interface).unwrap();
        }
        let specification = self.specification(&format!("{name} <- pure ({anchor})"), imports);
        let settled = self
            .source
            .with_source_compiler_transaction(
                tidepool_toolchain::artifacts::CompileWorkload::Foreground,
                tidepool_runtime::CompilerTransactionCancellation::new(),
                None,
                |_| {},
                |owner, _| {
                    prepare_world_cell_plan_admitted(
                        owner,
                        preparation.world(),
                        specification,
                        None,
                    )
                },
            )
            .unwrap();
        let plan = finish_world_cell_plan_admitted(settled).unwrap();
        let admission = source_action(session, &self.source, |session, _| {
            session.admit_world_planned_private_host_carrier(
                preparation,
                plan,
                name.to_owned(),
                expected,
                digest,
            )
        })
        .unwrap();
        let (binder, compiled, reservation) = self.consume(session, admission, name);
        assert!(
            HostCarrier::from_compiled(&binder, compiled.code(), expected).is_err(),
            "checked authority cannot become a reusable generic embedding carrier"
        );
        let carrier =
            HostCarrier::from_checked(reservation, binder, compiled.into_code(), expected).unwrap();
        session.retain_host_binding_prototype(&carrier).unwrap();
        carrier
    }

    fn mount(
        &self,
        session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        carrier: &tidepool_runtime::session::HostCarrier,
        payload: tidepool_runtime::session::HostPayload<'_>,
    ) -> BoundBinder {
        let binder = carrier.binding().unwrap().clone();
        let before = tidepool_extract_cmd::extract_spawn_count();
        let mut lease = source_action(session, &self.source, |session, report| {
            session.mount_private_visible_host_input(&self.execution, carrier, payload, report)
        })
        .unwrap();
        lease.accept().unwrap();
        assert_eq!(
            tidepool_extract_cmd::extract_spawn_count(),
            before,
            "mounting the accepted payload consumes its issued interface without compiling"
        );
        binder
    }

    fn observe(
        &self,
        session: &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
        text: &str,
        imports: &str,
    ) -> ResidentOutcome {
        let (binder, compiled, reservation) = self.compile(session, text, imports);
        let outcome = source_action(session, &self.source, |session, report| {
            session.run_observation_with_sites(
                compiled.code(),
                &binder,
                reservation.generation(),
                reservation.item().expression_lift().unwrap()
                    == Some(tidepool_toolchain::checked_cell::CheckedExpressionLift::Effectful),
                report,
            )
        })
        .unwrap();
        assert_eq!(
            reservation
                .prefix()
                .snapshot()
                .compiler_prefix()
                .next_item(),
            1
        );
        outcome
    }
}

impl Notebook {
    fn new() -> Self {
        Self::with_effects(&[])
    }

    /// [`Self::with_effects`], but the declaration plane's include directory
    /// IS `root` (rather than a nested `decl-lib` subdirectory) -- the
    /// production wiring (`SessionLib::open(id, session_root, ..)` in
    /// `exomonad-actor`'s workbench). A declaration turn's own GHC downsweep
    /// then resolves `root`-rooted `Val.G<g>` host-carrier stubs the same
    /// way a later cell's does, so a decl can capture a stub-mounted name.
    fn with_shared_root(decls: &[tidepool_mcp::EffectDecl]) -> Self {
        eval_harness::require_extract();
        let effects = TestEffectSurface::minimal(decls).expect("materialize effect surface");
        let preamble = effects.preamble().to_owned();
        let effect_stack = effects.row().to_owned();
        let mut include = effects.include_paths().to_vec();
        let root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            tidepool_repr::SessionId(1),
            root.path(),
            ModuleEnv::standalone_default(),
        )
        .expect("open decl plane")
        .with_validation_include(vec![eval_harness::prelude_path()]);
        include.push(lib.include_dir().to_path_buf());
        let mut session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let source = session
            .mint_isolated_scope_with_lease()
            .expect("issue the notebook's source owner");
        Self {
            session,
            source,
            preamble,
            effect_stack,
            include,
            root,
            injected: Vec::new(),
            generation: 0,
        }
    }

    fn with_effects(decls: &[tidepool_mcp::EffectDecl]) -> Self {
        eval_harness::require_extract();
        let effects = TestEffectSurface::minimal(decls).expect("materialize effect surface");
        let preamble = effects.preamble().to_owned();
        let effect_stack = effects.row().to_owned();
        let mut include = effects.include_paths().to_vec();
        let root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            tidepool_repr::SessionId(1),
            root.path().join("decl-lib"),
            ModuleEnv::standalone_default(),
        )
        .expect("open decl plane")
        .with_validation_include(vec![eval_harness::prelude_path()]);
        include.push(lib.include_dir().to_path_buf());
        let mut session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let source = session
            .mint_isolated_scope_with_lease()
            .expect("issue the notebook's source owner");
        Self {
            session,
            source,
            preamble,
            effect_stack,
            include,
            root,
            injected: Vec::new(),
            generation: 0,
        }
    }

    fn templates(&self) -> Vec<TurnTemplate> {
        resident_workbench_templates(
            &self.preamble,
            &self.effect_stack,
            &self.injected.join("\n"),
        )
    }

    fn compile(&mut self, text: &str) -> TurnResult {
        self.generation += 1;
        let retained = self.session.prepared_retained();
        let templates = self.templates();
        let include: Vec<&Path> = self.include.iter().map(PathBuf::as_path).collect();
        tidepool_testing::with_settlement(|settlement| {
            run_turn(
                TurnRequest {
                    exact_context: None,
                    session_id: None,
                    turn_text: text,
                    templates: &templates,
                    include: &include,
                    session_root: self.root.path(),
                    inject_modules: &self.injected,
                    gen: self.generation,
                    verdict: None,
                    retained_imports: &retained,
                },
                settlement,
            )
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}\n{}",
                tidepool_runtime::classify_compile(&failure.error).message,
                failure
                    .attempted_source
                    .as_deref()
                    .unwrap_or("<no attempted source>")
            )
        })
    }

    fn prepare_expression(&mut self, text: &str) -> CompiledTurn {
        let TurnResult::Expr { compiled, .. } = self.compile(text) else {
            panic!("{text:?} did not classify as an expression");
        };
        compiled
    }

    /// Compile against the session's current source view while giving the
    /// extractor every still-live value interface needed to link retained
    /// closures. Shadowed generations stay injected but are not imported
    /// unqualified, so a rebound name remains unambiguous to new source.
    fn compile_in_current_value_view(&mut self, text: &str) -> TurnResult {
        self.generation += 1;
        let imports = self
            .session
            .current_val_modules()
            .into_iter()
            .map(|module| format!("{module}\n"))
            .collect::<String>();
        let templates = resident_workbench_templates(&self.preamble, &self.effect_stack, &imports);
        let include: Vec<&Path> = self.include.iter().map(PathBuf::as_path).collect();
        let injected = self.session.inject_val_modules();
        let retained = self.session.prepared_retained();
        tidepool_testing::with_settlement(|settlement| {
            run_turn(
                TurnRequest {
                    exact_context: None,
                    session_id: None,
                    turn_text: text,
                    templates: &templates,
                    include: &include,
                    session_root: self.root.path(),
                    inject_modules: &injected,
                    gen: self.generation,
                    verdict: None,
                    retained_imports: &retained,
                },
                settlement,
            )
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}\n{}",
                tidepool_runtime::classify_compile(&failure.error).message,
                failure
                    .attempted_source
                    .as_deref()
                    .unwrap_or("<no attempted source>")
            )
        })
    }

    /// Install the immutable artifact afresh on every turn. Compilation is
    /// shared; mutable heap state and program retirement are still exercised.
    fn expression(&mut self, compiled: &CompiledTurn) {
        let outcome = self
            .source_action(|session, settlement| {
                session.run_with_sites("residency_expression", compiled.code(), settlement)
            })
            .expect("prepared expression runs");
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "{outcome:?}"
        );
    }

    fn source_action<T>(
        &mut self,
        action: impl FnOnce(
            &mut ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
            &mut dyn FnMut(tidepool_runtime::CompilerTransactionClose),
        ) -> T,
    ) -> T {
        source_action(&mut self.session, &self.source, action)
    }

    fn bind(&mut self, text: &str) -> BoundBinder {
        let TurnResult::Bind {
            bound, compiled, ..
        } = self.compile(text)
        else {
            panic!("{text:?} did not classify as a bind");
        };
        let [binder] = bound.as_slice() else {
            panic!("{text:?} bound {} names", bound.len());
        };
        let generation = Generation(self.generation);
        let outcome = self
            .source_action(|session, settlement| {
                session.run_bind_with_sites(
                    "residency_bind",
                    compiled.code(),
                    binder,
                    generation,
                    settlement,
                )
            })
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "{text:?} did not complete: {outcome:?}"
        );
        self.injected.push(binder.module.clone());
        binder.clone()
    }
}

#[test]
fn structural_resume_classifies_rejection_and_consumed_failure() {
    use tidepool_bridge::{BridgeError, HaskellVisitor, ToHaskell};
    use tidepool_repr::DataConTable;
    use tidepool_runtime::session::{PreparedRuntimeError, ResidentError, ResidentResumeError};

    struct MalformedAnswer;
    impl tidepool_bridge::sealed::ToHaskellSealed for MalformedAnswer {}
    impl ToHaskell for MalformedAnswer {
        fn visit(
            &self,
            table: &DataConTable,
            visitor: &mut dyn HaskellVisitor,
        ) -> Result<(), BridgeError> {
            let unit = tidepool_bridge::get_qualified(table, "GHC.Tuple.()", 0)
                .ok_or_else(|| BridgeError::UnknownDataConName("GHC.Tuple.()".into()))?;
            visitor.begin_constructor(unit, 0)?;
            Err(BridgeError::UnknownDataConName(
                "missing response metadata".into(),
            ))
        }
    }

    let mut notebook = Notebook::with_effects(&[tidepool_mcp::console_decl()]);
    let success = notebook.prepare_expression("say \"pause\" >> pure (42 :: Int)");
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .source_action(|session, settlement| {
            session.run_with_sites("response_rejection", success.code(), settlement)
        })
        .unwrap()
    else {
        panic!("Console request must suspend");
    };
    let roots = notebook.session.persistent_roots_count();
    let error = tidepool_testing::with_settlement(|settlement| {
        notebook
            .session
            .resume_classified(hole.clone(), MalformedAnswer, settlement)
    })
    .unwrap_err();
    assert!(
        matches!(error, ResidentResumeError::Rejected(ResidentError::Prepared(
        PreparedRuntimeError::AnswerRejected { source: BridgeError::UnknownDataConName(ref name), .. }
    )) if name == "missing response metadata"),
        "{error:?}"
    );
    assert_eq!(notebook.session.parked_holes(), vec![hole.cont_id()]);
    assert_eq!(notebook.session.persistent_roots_count(), roots);
    assert!(matches!(
        tidepool_testing::with_settlement(|settlement| notebook.session.resume_classified(
            hole,
            (),
            settlement
        ))
        .unwrap(),
        ResidentOutcome::Completed { .. }
    ));
    assert!(notebook.session.parked_holes().is_empty());

    let failure = notebook
        .prepare_expression("say \"pause\" >> (error \"after response\" :: Eff '[Console] Int)");
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .source_action(|session, settlement| {
            session.run_with_sites("consumed_response", failure.code(), settlement)
        })
        .unwrap()
    else {
        panic!("Console request must suspend before its failure");
    };
    let error = tidepool_testing::with_settlement(|settlement| {
        notebook.session.resume_classified(hole, (), settlement)
    })
    .unwrap_err();
    assert!(
        matches!(error, ResidentResumeError::Consumed(_)),
        "{error:?}"
    );
    assert!(notebook.session.parked_holes().is_empty());
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .source_action(|session, settlement| {
            session.run_with_sites("after_consumed_failure", success.code(), settlement)
        })
        .unwrap()
    else {
        panic!("the session must remain usable after a consumed failure");
    };
    assert!(matches!(
        tidepool_testing::with_settlement(|settlement| notebook.session.resume_classified(
            hole,
            (),
            settlement
        ))
        .unwrap(),
        ResidentOutcome::Completed { .. }
    ));
}

#[test]
fn custody_resume_classifies_rejected_frame_and_consumed_failure() {
    use tidepool_runtime::session::ResidentResumeError;

    let mut notebook = Notebook::with_effects(&[tidepool_mcp::console_decl()]);
    notebook.bind("held <- pure ()");
    let success = notebook.prepare_expression("say \"pause\" >> pure (42 :: Int)");
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .source_action(|session, settlement| {
            session.run_with_sites("framed_rejection", success.code(), settlement)
        })
        .unwrap()
    else {
        panic!("Console request must suspend");
    };
    let held = notebook
        .session
        .retain_binding_custody("held")
        .expect("retain independent binding custody")
        .unwrap();
    let error = tidepool_testing::with_settlement(|settlement| {
        notebook.session.resume_framed_custody_classified(
            hole.clone(),
            &held,
            tidepool_repr::DataConId(u64::MAX),
            Vec::new(),
            settlement,
        )
    })
    .unwrap_err();
    assert!(
        matches!(error, ResidentResumeError::Rejected(_)),
        "{error:?}"
    );
    assert_eq!(notebook.session.parked_holes(), vec![hole.cont_id()]);
    assert!(matches!(
        tidepool_testing::with_settlement(|settlement| notebook
            .session
            .resume_handle_classified(hole, held, settlement))
        .unwrap(),
        ResidentOutcome::Completed { .. }
    ));

    let failure = notebook
        .prepare_expression("say \"pause\" >> (error \"after handle\" :: Eff '[Console] Int)");
    let ResidentOutcome::Suspended { hole, .. } = notebook
        .source_action(|session, settlement| {
            session.run_with_sites("consumed_handle", failure.code(), settlement)
        })
        .unwrap()
    else {
        panic!("Console request must suspend before its failure");
    };
    let held = notebook
        .session
        .retain_binding_custody("held")
        .expect("retain independent binding custody")
        .unwrap();
    let error = tidepool_testing::with_settlement(|settlement| {
        notebook
            .session
            .resume_handle_classified(hole, held, settlement)
    })
    .unwrap_err();
    assert!(
        matches!(error, ResidentResumeError::Consumed(_)),
        "{error:?}"
    );
    assert!(notebook.session.parked_holes().is_empty());
}

/// Parcel 5b: a value crosses two independent [`ResidentSession`]s (their
/// own machines, sharing one [`tidepool_runtime::session::ImageRegistry`])
/// through [`ResidentSession::export_custody`]/[`ResidentSession::import_parcel`].
/// Both sessions install the same immutable compiler output in distinct
/// machines. A heap-allocated value is copied on export; a static top retains
/// its exact source installation instance in the parcel.
#[test]
fn parcel_crosses_two_resident_sessions_sharing_one_image_registry() {
    use std::sync::Arc;
    use tidepool_runtime::session::ImageRegistry;

    let mut left = Notebook::new();
    let mut right = Notebook::new();

    // Bootstrap independent machines from one immutable compiler output.
    let warm = left.prepare_expression("pure (0 :: Int)");
    left.expression(&warm);
    right.expression(&warm);

    let registry = Arc::new(ImageRegistry::new());
    left.session.set_image_registry(Arc::clone(&registry));
    right.session.set_image_registry(Arc::clone(&registry));

    // A bound value's own top is a static CAF (every `bind()`-produced
    // `Val.G<gen>` top is), so export carries its exact static region by
    // address (`Parcel::bytes() == 0`). The registry hit shares compiled
    // code, while each install owns a distinct static region. Import must
    // admit the source instance carried by the parcel even when the sibling
    // already installed that same compiled code.
    let TurnResult::Bind {
        bound, compiled, ..
    } = left.compile("held <- pure (999999 :: Int)")
    else {
        panic!("held must compile as a binding");
    };
    let [binder] = bound.as_slice() else {
        panic!("held must have one original binder");
    };
    assert!(compiled.prepared().globals().iter().all(|global| {
        tidepool_repr::SessionModule::from_module_name(&global.identity.module)
            .is_none_or(|module| module.kind != tidepool_repr::SessionModuleKind::Lib)
    }));
    let generation = Generation(left.generation);
    let install = |notebook: &mut Notebook| {
        let outcome = notebook
            .source_action(|session, settlement| {
                session.run_bind_with_sites(
                    "shared_held",
                    compiled.code(),
                    binder,
                    generation,
                    settlement,
                )
            })
            .expect("install the original immutable binding in an independent machine");
        assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
        notebook.generation = generation.0;
        notebook.injected.push(binder.module.clone());
    };
    install(&mut left);
    let hits_before_sibling = registry.hits();
    install(&mut right);
    assert!(
        registry.hits() > hits_before_sibling,
        "the sibling installation must reuse the original compiled image"
    );
    let root = tidepool_codegen::scope::ScopeId::ROOT;
    let left_binding = left.session.current_binding_in(root, "held").unwrap();
    let right_binding = right.session.current_binding_in(root, "held").unwrap();
    assert_eq!(left_binding.0, right_binding.0, "same original binder ID");
    assert_eq!(left_binding.1, right_binding.1, "same original module");
    let custody = left
        .session
        .retain_binding_custody("held")
        .expect("retain independent binding custody")
        .expect("the bound name resolves to a live custody token");
    let before = left
        .session
        .render_retained_preview(&custody, 64)
        .expect("left can render its own custody");

    let left_handles_before = left.session.value_handle_count();
    let parcel = left
        .session
        .export_custody(custody)
        .expect("the bound value exports as a parcel");
    assert_eq!(
        parcel.bytes(),
        0,
        "the parcel retains the static source instance"
    );
    assert_eq!(
        left.session.value_handle_count(),
        left_handles_before - 1,
        "export releases the original rooted handle"
    );

    // Checked later-cell use is covered independently by
    // checked_binding_survives_export_and_executes_in_later_cell: a checked
    // execution prefix belongs to one machine and cannot be shared here.
    assert!(left
        .session
        .current_binding_in(tidepool_codegen::scope::ScopeId::ROOT, "held")
        .is_some());

    let right_handles_before = right.session.value_handle_count();
    let right_programs_before = right.session.residency().unwrap().programs;
    let right_visibility_before = right.session.public_visibility_snapshot_in(root).unwrap();
    let imported = right
        .session
        .import_parcel(parcel, tidepool_runtime::session::RealmId::ROOT)
        .expect("the parcel installs its source instance in the sibling machine");
    assert_eq!(
        right.session.value_handle_count(),
        right_handles_before + 1,
        "a self-contained parcel mints its value handle without declaration-binding handles"
    );
    assert!(
        right.session.residency().unwrap().programs > right_programs_before,
        "import retains the left installation, even with the same code installed on the right"
    );
    assert_eq!(
        right.session.current_binding_in(root, "held").as_ref(),
        Some(&right_binding)
    );
    assert_eq!(
        right.session.public_visibility_snapshot_in(root).unwrap(),
        right_visibility_before,
        "native instance custody does not publish source bindings"
    );
    drop(left);

    let after = right
        .session
        .render_retained_preview(&imported, 64)
        .expect("right can render the imported custody");
    assert_eq!(before, after, "the value round-trips byte-for-byte");
    assert_eq!(after, "999999");

    // There are no declaration-binding imports to retain after releasing the
    // transferred value. The receiver's independently installed binding survives.
    drop(imported);
    assert_eq!(right.session.value_handle_count(), right_handles_before);
    assert_eq!(
        right.session.current_binding_in(root, "held").as_ref(),
        Some(&right_binding)
    );
}

/// Fixture mirror of `PreparedEngine::MAJOR_COLLECTION_INSTALL_INTERVAL`
/// (`tidepool/runtime/src/session/prepared.rs`) -- see the module doc for
/// why this file cannot read the real constant.
const N: usize = 4;

/// Assert every `ResidencyCounts` field, plus `old_bytes`, is identical to
/// `previous` -- the flatness a completed major collection should leave
/// behind, since it folds every earlier collection boundary's counters back
/// to the same live set (no live bindings changed in between).
fn assert_counts_flat(
    label: &str,
    counts: tidepool_codegen::prepared_program::ResidencyCounts,
    previous: tidepool_codegen::prepared_program::ResidencyCounts,
    old_bytes: usize,
    previous_old_bytes: usize,
) {
    assert_eq!(counts, previous, "{label}: counts moved across collections");
    assert_eq!(
        old_bytes, previous_old_bytes,
        "{label}: old_bytes moved across collections"
    );
}

#[test]
fn root_installation_checks_the_original_source_gate_before_mutation() {
    use tidepool_runtime::session::{
        ResidentError, SessionError, SourceAdmissionError, SourceAdmissionPhase,
    };

    let mut notebook = Notebook::new();
    let expression = notebook.prepare_expression("1 + (2 :: Int)");
    notebook.expression(&expression);
    let before = notebook.session.residency();
    let original_scope = notebook.source.scope();
    let _fence = notebook
        .source
        .source_admission()
        .fence()
        .expect("close only the original source gate");

    let error = notebook
        .source_action(|session, settlement| {
            session.run_with_sites("closed_original_source", expression.code(), settlement)
        })
        .expect_err("an open ROOT receiver cannot replace the original source authority");
    assert!(
        matches!(
            error,
            ResidentError::Session(SessionError::SourceAdmission(SourceAdmissionError::Closed {
                scope,
                phase: SourceAdmissionPhase::Fenced { .. },
            })) if scope == original_scope
        ),
        "{error:?}"
    );
    assert_eq!(notebook.session.residency(), before);
}

/// Twenty installations of one prepared constant expression on the prepared route, each installing
/// exactly one program: `quiesce_and_collect` only actually collects every
/// `N`th install (see the module doc), so `programs` is checked against the
/// amortized bound every turn, and exact flatness across every counter --
/// `old_bytes` included -- is only checked collection-boundary to
/// collection-boundary (every `N`th turn). The first boundary is a warm-up:
/// the first few installs may still be settling machine bootstrap/site-index
/// bookkeeping, and descriptor-arena compaction commits before retirement
/// per the lifetime contract's decision 7, so a retiring program's bytes can
/// still be one collection behind its other counters at that point.
/// Comparisons start at the second boundary, once both have stabilized.
///
/// A binding is then introduced (`x <- pure 1`) and twelve more expression
/// turns run importing nothing new: the same amortized-bound-every-turn,
/// flat-at-every-boundary structure applies, now against the post-bind
/// baseline -- the bound bind's own program does not accumulate one
/// retained program per subsequent collection.
#[test]
fn prepared_session_residency_stays_bounded_across_many_turns() {
    let mut notebook = Notebook::new();

    // A multiple of `N`, so the phase ends exactly on a collection boundary
    // and the bind below is the first install of the next window.
    const TOTAL: usize = 20;
    let live_bindings = 0;
    let mut boundary_counts: Option<tidepool_codegen::prepared_program::ResidencyCounts> = None;
    let mut boundary_old_bytes: Option<usize> = None;
    let mut boundaries_seen = 0;
    let expression = notebook.prepare_expression("1 + (2 :: Int)");
    for i in 0..TOTAL {
        notebook.expression(&expression);
        let counts = notebook
            .session
            .residency()
            .expect("the prepared route reports residency counts");
        assert!(
            counts.programs <= live_bindings + 1 + N,
            "turn {i}: programs={} exceeds the amortized bound ({live_bindings} live bindings + 1 + N)",
            counts.programs
        );
        // A collection boundary: `installs_since_major` (turn number, since
        // each turn installs exactly one program) hit the window.
        if (i + 1) % N == 0 {
            boundaries_seen += 1;
            let old_bytes = notebook
                .session
                .old_bytes()
                .expect("the prepared route reports old-space bytes");
            if boundaries_seen > 1 {
                assert_counts_flat(
                    &format!("turn {i} (boundary {boundaries_seen})"),
                    counts,
                    boundary_counts.expect("a prior boundary was recorded"),
                    old_bytes,
                    boundary_old_bytes.expect("a prior boundary was recorded"),
                );
            }
            boundary_counts = Some(counts);
            boundary_old_bytes = Some(old_bytes);
        }
    }
    assert!(
        boundaries_seen >= 2,
        "expected at least two collection boundaries in {TOTAL} turns at N={N}"
    );

    // Introduce a live binding, then keep running: the bound value's
    // producing program may or may not stay resident (it depends on whether
    // the binding's value keeps any of its code reachable), but no further
    // program should accumulate collection over collection.
    notebook.bind("x <- pure 1");
    let live_bindings = 1;

    const POST_BIND: usize = 12;
    let mut boundary_counts: Option<tidepool_codegen::prepared_program::ResidencyCounts> = None;
    let mut boundary_old_bytes: Option<usize> = None;
    let mut boundaries_seen = 0;
    let expression = notebook.prepare_expression("x + (2 :: Int)");
    for i in 0..POST_BIND {
        notebook.expression(&expression);
        let counts = notebook
            .session
            .residency()
            .expect("the prepared route reports residency counts");
        assert!(
            counts.programs <= live_bindings + 1 + N,
            "post-bind turn {i}: programs={} exceeds the amortized bound ({live_bindings} live binding + 1 + N)",
            counts.programs
        );
        // The bind turn installed one program right after the pre-bind
        // phase's final boundary, so this window's installs run one ahead
        // of the turn index: the collection lands where `i + 2` fills it.
        if (i + 2) % N == 0 {
            boundaries_seen += 1;
            let old_bytes = notebook
                .session
                .old_bytes()
                .expect("the prepared route reports old-space bytes");
            if boundaries_seen > 1 {
                assert_counts_flat(
                    &format!("post-bind turn {i} (boundary {boundaries_seen})"),
                    counts,
                    boundary_counts.expect("a prior boundary was recorded"),
                    old_bytes,
                    boundary_old_bytes.expect("a prior boundary was recorded"),
                );
            }
            boundary_counts = Some(counts);
            boundary_old_bytes = Some(old_bytes);
        }
    }
    assert!(
        boundaries_seen >= 2,
        "expected at least two post-bind collection boundaries in {POST_BIND} turns at N={N}"
    );
}

/// A large promotion trips `PreparedEngine::major_collection_due`'s live
/// old-space growth check (`tidepool/runtime/src/session/prepared.rs`)
/// well before the install-count window (`N`) would close on its own.
///
/// The growth check is disabled until a real baseline exists (the very
/// first major collection is always gated by the install count alone --
/// see that function's doc), so this first runs exactly `N` tiny turns to
/// land on a collection boundary and establish a nonzero `old_bytes`
/// baseline. The bind below then FORCES a 200,000-element list's whole
/// spine (`length ys \`seq\` ys`) before returning it, so the list is
/// actually built rather than left as an unevaluated `enumFromTo` thunk --
/// tenured prepared bindings are kept exactly as given, never deep-forced
/// (`ResidentSession::current_binding_in`'s doc), so an unforced `pure
/// [1..200000]` binds a thunk of a few words and never promotes anything.
/// The forced list is well past the 1 MiB / 50% growth threshold; a
/// following turn or two should fold that promotion into `old_bytes` even
/// though only one or two programs have installed since the baseline --
/// far short of `N`.
#[test]
fn prepared_session_large_promotion_triggers_an_early_major_collection() {
    let mut notebook = Notebook::new();

    let expression = notebook.prepare_expression("1 + (2 :: Int)");
    for _ in 0..N {
        notebook.expression(&expression);
    }
    let old_bytes_before = notebook
        .session
        .old_bytes()
        .expect("the prepared route reports old-space bytes");

    notebook.bind("xs <- (let ys = [1..200000 :: Int] in seq (length ys) (pure ys))");

    let mut collected_within = None;
    for i in 0..2 {
        notebook.expression(&expression);
        let old_bytes_now = notebook
            .session
            .old_bytes()
            .expect("the prepared route reports old-space bytes");
        if old_bytes_now >= old_bytes_before + 1024 * 1024 {
            collected_within = Some(i + 1);
            break;
        }
    }
    assert!(
        collected_within.is_some(),
        "expected a major collection to fold the large list's promoted bytes into `old_bytes` \
         within 2 turns of binding it (stayed at {old_bytes_before}); the live old-space growth \
         trigger in `PreparedEngine::major_collection_due` should fire well before the next \
         install-count boundary at N={N}"
    );
}

#[test]
fn rust_session_var_id_matches_extract_minted_bound_binder_var_id() {
    let mut notebook = Notebook::new();
    let binder = notebook.bind("parityVal <- pure (1 :: Int)");
    let expected = tidepool_codegen::prepared_program::session_var_id(&binder.module, &binder.name);
    assert_eq!(
        expected, binder.var_id,
        "Rust-minted session_var_id({:?}, {:?}) = 0x{expected:016x} does not match \
         the extract-minted BoundBinder.var_id 0x{:016x}",
        binder.module, binder.name, binder.var_id
    );
}

#[test]
fn resident_turn_carries_worker_certified_import_owners() {
    let mut notebook = Notebook::new();

    let checked = CheckedExecution::start(
        &mut notebook.session,
        &notebook.include,
        &notebook.preamble,
        &notebook.effect_stack,
    );
    for text in [
        "source <- pure (1 :: Int)",
        "certified <- pure (source + 1)",
    ] {
        let (binder, compiled, reservation) = checked.compile(&mut notebook.session, text, "");
        let certification = compiled.certification().expect("checked turn is certified");
        assert_eq!(
            certification.target_owners.len(),
            compiled.prepared().globals().len()
        );
        if binder.name == "certified" {
            assert!(
                !compiled.prepared().globals().is_empty(),
                "second checked turn retains the first value"
            );
        }
        let outcome = source_action(&mut notebook.session, &checked.source, |session, report| {
            session.run_bind_with_sites(
                &binder.name,
                compiled.code(),
                &binder,
                reservation.generation(),
                report,
            )
        })
        .expect("checked certified turn runs");
        assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
        assert_eq!(
            reservation
                .prefix()
                .snapshot()
                .compiler_prefix()
                .next_item(),
            1
        );
    }
}

#[test]
fn host_carrier_mounts_json_text_and_job_payloads_from_one_compile_each() {
    use tidepool_runtime::session::{HostBindingType, HostPayload};
    let mut notebook = Notebook::new();
    let checked = CheckedExecution::start(
        &mut notebook.session,
        &notebook.include,
        &notebook.preamble,
        &notebook.effect_stack,
    );
    let imports = "qualified Data.Text as TidepoolHostText\n\
        qualified Data.Text.Internal as TidepoolHostTextInternal\n\
        qualified GHC.Exts as TidepoolHostExts\n\
        qualified Tidepool.Command.Types as TidepoolHostJob\n";
    let json_anchor = "object [\"anchor\" .= toJSON [Aeson.String \"\", Aeson.Number (Aeson.scientific 0 0), Aeson.Bool True, Aeson.Null]]";
    let carrier_a = checked.host_carrier(
        &mut notebook.session,
        "carriedA",
        HostBindingType::JSON_VALUE,
        json_anchor,
        imports,
    );
    let before_refusal = source_action(&mut notebook.session, &checked.source, |session, _| {
        session.prepare_private_host_carrier(checked.execution.clone(), [19; 32])
    })
    .unwrap();
    let visibility_before_refusal = notebook
        .session
        .public_visibility_snapshot_in(checked.execution.private_scope())
        .unwrap();
    let refused = source_action(&mut notebook.session, &checked.source, |session, report| {
        session.mount_private_visible_host_input(
            &checked.execution,
            &carrier_a,
            HostPayload::Job("wrong nominal payload"),
            report,
        )
    });
    assert!(refused.is_err(), "wrong payload refuses its private mount");
    let after_refusal = source_action(&mut notebook.session, &checked.source, |session, _| {
        session.prepare_private_host_carrier(checked.execution.clone(), [19; 32])
    })
    .unwrap();
    assert_eq!(
        before_refusal.world().logical_id(),
        after_refusal.world().logical_id()
    );
    assert_eq!(
        notebook
            .session
            .public_visibility_snapshot_in(checked.execution.private_scope())
            .unwrap(),
        visibility_before_refusal,
    );
    // The refusal retained the original reservation for its matching payload.
    let binder_a = checked.mount(
        &mut notebook.session,
        &carrier_a,
        HostPayload::Json(&serde_json::json!({"tag": "A"})),
    );
    // Each new binding receives its own issued interface. Reusing JSON keeps
    // the same native constructor prototype, without recompiling that anchor.
    let carrier_b = checked.host_carrier(
        &mut notebook.session,
        "carriedB",
        HostBindingType::JSON_VALUE,
        json_anchor,
        imports,
    );
    let binder_b = checked.mount(
        &mut notebook.session,
        &carrier_b,
        HostPayload::Json(&serde_json::json!({"tag": "B"})),
    );
    assert_ne!(binder_a.module, binder_b.module);
    let text_carrier = checked.host_carrier(&mut notebook.session, "carriedText", HostBindingType::TEXT,
        "case TidepoolHostExts.noinline (TidepoolHostText.pack \"\") of TidepoolHostTextInternal.Text bytes offset length -> TidepoolHostTextInternal.Text bytes offset length", imports);
    let binder_text = checked.mount(
        &mut notebook.session,
        &text_carrier,
        HostPayload::Text("hello-carrier-text"),
    );
    let job_carrier = checked.host_carrier(
        &mut notebook.session,
        "carriedJob",
        HostBindingType::COMMAND_JOB,
        "TidepoolHostJob.Job (TidepoolHostText.pack \"\")",
        imports,
    );
    let binder_job = checked.mount(
        &mut notebook.session,
        &job_carrier,
        HostPayload::Job("job-command"),
    );
    let visible = notebook
        .session
        .public_visibility_snapshot_in(checked.execution.private_scope())
        .unwrap();
    for binder in [&binder_a, &binder_b, &binder_text, &binder_job] {
        assert!(visible
            .bindings
            .iter()
            .any(|(name, id)| name == &binder.name
                && *id == tidepool_repr::SessionVarId::from_extract(binder.var_id)));
    }
    let outcome = checked.observe(&mut notebook.session, "case (carriedA, carriedB, carriedText, carriedJob) of \
         { (Aeson.Object a, Aeson.Object b, TidepoolHostTextInternal.Text _ _ _, TidepoolHostJob.Job jobText) -> \
         if Map.lookup \"tag\" a == Just (Aeson.String \"A\") \
            && Map.lookup \"tag\" b == Just (Aeson.String \"B\") \
            && carriedText == TidepoolHostText.pack \"hello-carrier-text\" \
            && jobText == TidepoolHostText.pack \"job-command\" \
         then () else error \"carrier payload mismatch\"; \
         _ -> error \"carrier constructor mismatch\" }", imports);
    assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
}

#[test]
fn retired_carrier_binding_captured_by_declaration_fails_to_compile_not_hang() {
    use std::sync::mpsc;
    use std::time::Duration;
    use tidepool_codegen::scope::ScopeId;
    use tidepool_runtime::session::{HostBindingType, HostCarrier, HostPayload};

    let mut notebook = Notebook::with_shared_root(&[]);

    let TurnResult::Bind {
        bound, compiled, ..
    } = notebook.compile_in_current_value_view(
        "carrierAnchor <- pure (object [\"anchor\" .= toJSON [Aeson.String \"\"]])",
    )
    else {
        panic!("anchor must compile as a bind");
    };
    let [anchor_binder] = bound.as_slice() else {
        panic!("anchor must produce exactly one binder");
    };
    let carrier =
        HostCarrier::from_compiled(anchor_binder, compiled.code(), HostBindingType::JSON_VALUE)
            .expect("compiler issued a complete reusable host representation");

    notebook.generation += 1;
    let gen = Generation(notebook.generation);
    let binder = source_action(
        &mut notebook.session,
        &notebook.source,
        |session, settlement| {
            session.mount_carrier_in(
                notebook.root.path(),
                ScopeId::ROOT,
                "capturedCarrier",
                gen,
                &carrier,
                HostPayload::Json(&serde_json::json!({"tag": "captured"})),
                settlement,
            )
        },
    )
    .expect("mount carrier binding");
    notebook.injected.push(binder.module.clone());

    let stub_path = notebook
        .root
        .path()
        .join(tidepool_repr::SessionModule::val(gen).relative_hs_path());
    assert!(stub_path.exists(), "mount must write its stub source");

    // A declaration captures the carrier binding by name -- `define_scoped`
    // auto-imports every currently-visible value's exact `Val.G<g>` module,
    // so the committed `Lib.G<g>` source now names this stub generation.
    source_action(
        &mut notebook.session,
        &notebook.source,
        |session, settlement| session.define_scoped(&["capturedUse = capturedCarrier"], settlement),
    )
    .expect("declaration captures the carrier binding");

    // Retire the carrier binding: it must both stop being excluded from
    // `--inject-val` bookkeeping under its own generation AND delete its
    // stub source, matching a real binding's eviction instead of leaving a
    // phantom stub the declaration module can still find.
    notebook
        .session
        .retire_host_binding_owner(notebook.root.path(), &binder);
    assert!(
        !stub_path.exists(),
        "retiring a captured carrier binding must delete its stub source"
    );

    // A later turn using the declaration must fail loudly with a compile
    // error -- never hang evaluating the retired stub's self-referential
    // body. `compile_in_current_value_view` panics on its own compile
    // failure, so the expected outcome is the compile thread panicking, not
    // returning a successful classification.
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = notebook.compile_in_current_value_view("capturedUse");
        tx.send(result).ok();
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(result) => panic!(
            "a turn referencing a retired carrier binding through a declaration must fail to \
             compile, not succeed: {result:?}"
        ),
        Err(mpsc::RecvTimeoutError::Timeout) => panic!(
            "compiling a turn that references a retired carrier binding through a declaration \
             hung instead of failing with a compile error"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // The compile thread panicked before sending a result --
            // `compile_in_current_value_view`'s own `unwrap_or_else(panic!)`
            // on a real compile failure, exactly the expected outcome.
            handle.join().expect_err(
                "the compile thread must have panicked from a compile failure, not from \
                 something else",
            );
        }
    }
}

/// A wrong payload cannot mutate a reusable carrier's session or write a stub.
#[test]
fn failed_carrier_mount_leaves_no_stub_source() {
    use tidepool_codegen::scope::ScopeId;
    use tidepool_runtime::session::{HostBindingType, HostCarrier, HostPayload, ResidentError};

    let mut notebook = Notebook::with_shared_root(&[]);

    let TurnResult::Bind {
        bound, compiled, ..
    } = notebook.compile_in_current_value_view(
        "carrierAnchor <- pure (object [\"anchor\" .= toJSON [Aeson.String \"\"]])",
    )
    else {
        panic!("anchor must compile as a bind");
    };
    let [anchor_binder] = bound.as_slice() else {
        panic!("anchor must produce exactly one binder");
    };
    let mut missing_root = anchor_binder.clone();
    missing_root.root_head = None;
    assert!(HostCarrier::from_compiled(
        &missing_root,
        compiled.code(),
        HostBindingType::JSON_VALUE
    )
    .is_err());
    let mut wrong_root = anchor_binder.clone();
    wrong_root
        .root_head
        .as_mut()
        .unwrap()
        .symbol
        .unit
        .push_str("-foreign");
    assert!(
        HostCarrier::from_compiled(&wrong_root, compiled.code(), HostBindingType::JSON_VALUE)
            .is_err()
    );
    assert!(CompiledTurn::from_prepared(
        std::sync::Arc::clone(compiled.prepared()),
        tidepool_repr::DataConTable::new(),
        compiled.warnings.clone(),
        compiled.sites().to_vec(),
    )
    .is_err());
    assert!(compiled.certification().is_some());
    let carrier =
        HostCarrier::from_compiled(anchor_binder, compiled.code(), HostBindingType::JSON_VALUE)
            .expect("compiler issued a complete reusable host representation");

    notebook.generation += 1;
    let gen = Generation(notebook.generation);
    let residency = notebook.session.residency();
    let before = notebook
        .session
        .public_visibility_snapshot_in(ScopeId::ROOT)
        .unwrap();
    let error = tidepool_testing::with_settlement(|settlement| {
        notebook.session.mount_carrier_in(
            notebook.root.path(),
            ScopeId::ROOT,
            "unmountable",
            gen,
            &carrier,
            HostPayload::Job("wrong nominal payload"),
            settlement,
        )
    })
    .expect_err("a wrong payload kind must refuse before mutation");
    assert!(matches!(error, ResidentError::UnsupportedCheckedTurn));

    assert_eq!(notebook.session.residency(), residency);
    assert_eq!(
        notebook
            .session
            .public_visibility_snapshot_in(ScopeId::ROOT)
            .unwrap(),
        before
    );
    assert!(notebook
        .session
        .current_binding_in(ScopeId::ROOT, "unmountable")
        .is_none());

    let stub_path = notebook
        .root
        .path()
        .join(tidepool_repr::SessionModule::val(gen).relative_hs_path());
    assert!(
        !stub_path.exists(),
        "a failed carrier mount must not leave its stub source on disk (mount error: {error})"
    );
}

/// MEDIUM regression: a fresh `PersistentSession` reopening a session root
/// resets `val_gen`/`stub_generations`, so a leftover stub `.hs` from a
/// prior incarnation could coexist with a REAL, `.hi`-backed bind that
/// reissues its generation number. `SessionLib::open` must sweep every
/// stale stub source under the root's `Tidepool/Session/Val` so none can be
/// found by the new incarnation.
#[test]
fn reopening_session_root_removes_stale_stub_sources() {
    let root = tempfile::tempdir().expect("session root");
    let stub_dir = root.path().join("Tidepool/Session/Val");
    std::fs::create_dir_all(&stub_dir).expect("stub directory");
    let stale_stub = stub_dir.join("G3.hs");
    std::fs::write(
        &stale_stub,
        "module Tidepool.Session.Val.G3 (leftover) where\n\
         leftover :: ()\n\
         leftover = ()\n",
    )
    .expect("write stale stub");
    assert!(stale_stub.exists());

    SessionLib::open(
        tidepool_repr::SessionId(1),
        root.path(),
        ModuleEnv::standalone_default(),
    )
    .expect("reopen session root");

    assert!(
        !stale_stub.exists(),
        "opening a session must sweep stale stub sources left by a prior incarnation"
    );
}

/// One incarnation's compile plumbing, sharing `root` with a sibling
/// incarnation (unlike `Notebook`, which always opens its own fresh
/// `tempdir`). Mirrors `Notebook::with_shared_root` field for field so the
/// two behave identically except for `id`/`root`.
struct Incarnation {
    session: ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    source: Arc<RuntimeLexicalScopeLease>,
    preamble: String,
    effect_stack: String,
    include: Vec<PathBuf>,
    root: PathBuf,
    injected: Vec<String>,
    generation: u64,
    id: tidepool_repr::SessionId,
    /// When false, every compile forwards `session_id: None` regardless of
    /// `id` -- the "before this feature" behavior, for the A/B measurement
    /// in `stub_growth_admission_compile_time`. `true` everywhere else.
    send_incarnation: bool,
}

impl Incarnation {
    fn open(id: tidepool_repr::SessionId, root: &Path) -> Self {
        let effects = TestEffectSurface::minimal(&[]).expect("materialize effect surface");
        let preamble = effects.preamble().to_owned();
        let effect_stack = effects.row().to_owned();
        let mut include = effects.include_paths().to_vec();
        let lib = SessionLib::open(id, root, ModuleEnv::standalone_default())
            .expect("open decl plane")
            .with_validation_include(vec![eval_harness::prelude_path()]);
        include.push(lib.include_dir().to_path_buf());
        include.push(root.to_path_buf());
        let mut session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        let source = session
            .mint_isolated_scope_with_lease()
            .expect("issue the incarnation's source owner");
        Self {
            session,
            source,
            preamble,
            effect_stack,
            include,
            root: root.to_path_buf(),
            injected: Vec::new(),
            generation: 0,
            id,
            send_incarnation: true,
        }
    }

    fn compile_in_current_value_view(&mut self, text: &str) -> TurnResult {
        self.generation += 1;
        let imports = self
            .session
            .current_val_modules()
            .into_iter()
            .map(|module| format!("{module}\n"))
            .collect::<String>();
        let templates = resident_workbench_templates(&self.preamble, &self.effect_stack, &imports);
        let include: Vec<&Path> = self.include.iter().map(PathBuf::as_path).collect();
        let injected = self.session.inject_val_modules();
        let retained = self.session.prepared_retained();
        tidepool_testing::with_settlement(|settlement| {
            run_turn(
                TurnRequest {
                    exact_context: None,
                    session_id: self.send_incarnation.then_some(self.id),
                    turn_text: text,
                    templates: &templates,
                    include: &include,
                    session_root: &self.root,
                    inject_modules: &injected,
                    gen: self.generation,
                    verdict: None,
                    retained_imports: &retained,
                },
                settlement,
            )
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}\n{}",
                tidepool_runtime::classify_compile(&failure.error).message,
                failure
                    .attempted_source
                    .as_deref()
                    .unwrap_or("<no attempted source>")
            )
        })
    }

    fn mount_json(
        &mut self,
        carrier: &tidepool_runtime::session::HostCarrier,
        name: &str,
        value: &serde_json::Value,
    ) -> tidepool_runtime::session::BoundBinder {
        use tidepool_codegen::scope::ScopeId;
        use tidepool_runtime::session::HostPayload;
        self.generation += 1;
        let gen = tidepool_repr::Generation(self.generation);
        let binder = source_action(&mut self.session, &self.source, |session, settlement| {
            session.mount_carrier_in(
                &self.root,
                ScopeId::ROOT,
                name,
                gen,
                carrier,
                HostPayload::Json(value),
                settlement,
            )
        })
        .expect("mount json carrier payload");
        self.injected.push(binder.module.clone());
        binder
    }

    fn json_anchor(&mut self) -> tidepool_runtime::session::HostCarrier {
        use tidepool_runtime::session::{HostBindingType, HostCarrier};
        let TurnResult::Bind {
            bound, compiled, ..
        } = self.compile_in_current_value_view(
            "carrierAnchor <- pure (object [\"anchor\" .= toJSON [Aeson.String \"\"]])",
        )
        else {
            panic!("json anchor must compile as a bind");
        };
        let [anchor_binder] = bound.as_slice() else {
            panic!("json anchor must produce exactly one binder");
        };
        HostCarrier::from_compiled(anchor_binder, compiled.code(), HostBindingType::JSON_VALUE)
            .expect("compiler issued a complete reusable host representation")
    }
}

/// Scratch measurement, not part of the default suite: admission-compile
/// wall time at stub count 10 vs. 200, with vs. without a session
/// incarnation identity (i.e. `sanitizeMemo`'s old unconditional
/// `Tidepool.Session.*` eviction vs. the new incarnation-scoped one), all
/// against ONE already-warm daemon process (set `$TIDEPOOL_EXTRACT_DAEMON_SOCKET`
/// to a private daemon; the shared battery daemon must never be used for
/// this). Run with `cargo test --release -- --ignored --nocapture
/// stub_growth_admission_compile_time`, set `MEASURE_INCARNATION=1` for the
/// "after" run and unset it for the "before" run (two separate invocations,
/// so each starts from a fresh, empty `GutsMemo`).
#[test]
#[ignore]
fn stub_growth_admission_compile_time() {
    eval_harness::require_extract();
    let use_incarnation = std::env::var("MEASURE_INCARNATION").is_ok();
    let root = tempfile::tempdir().expect("session root");
    let mut incarnation = Incarnation::open(tidepool_repr::SessionId(1), root.path());
    incarnation.send_incarnation = use_incarnation;
    let carrier = incarnation.json_anchor();

    const CHECKPOINTS: &[usize] = &[10, 200];
    let mut next_checkpoint = 0;
    // One admission compile PER mount, matching the real workload: a bash
    // call mounts a stub, then the next cell's admission compile sees it.
    // Compiling only at sparse checkpoints (against a cold, never-before-
    // compiled batch of modules) would make every checkpoint a first-time
    // miss regardless of incarnation and hide exactly the effect being
    // measured -- the point is that stubs 1..N-1 are HITS by the time
    // stub N's admission compile runs, in the "after" condition.
    for i in 1..=200usize {
        let name = format!("carried{i}");
        incarnation.mount_json(&carrier, &name, &serde_json::json!({"tag": i}));
        let t0 = std::time::Instant::now();
        let _ = incarnation.compile_in_current_value_view("()");
        let elapsed = t0.elapsed();
        if next_checkpoint < CHECKPOINTS.len() && i == CHECKPOINTS[next_checkpoint] {
            eprintln!(
                "stub_growth_admission_compile_time incarnation={} stubs={} wall_ms={}",
                use_incarnation,
                i,
                elapsed.as_millis()
            );
            next_checkpoint += 1;
        }
    }
}

/// Independent session issuers may reuse a module generation at the same disk
/// root. Their accepted values and compiler Worlds must remain disjoint.
struct CheckedIncarnation {
    inner: Incarnation,
    checked: CheckedExecution,
}

impl CheckedIncarnation {
    fn open(id: tidepool_repr::SessionId, root: &Path) -> Self {
        let mut inner = Incarnation::open(id, root);
        let checked = CheckedExecution::start(
            &mut inner.session,
            &inner.include,
            &inner.preamble,
            &inner.effect_stack,
        );
        Self { inner, checked }
    }

    fn mount_json(&mut self, name: &str, value: &serde_json::Value) -> BoundBinder {
        let carrier = self.checked.host_carrier(
            &mut self.inner.session,
            name,
            tidepool_runtime::session::HostBindingType::JSON_VALUE,
            "object [\"anchor\" .= toJSON [Aeson.String \"\"]]",
            "",
        );
        self.checked.mount(
            &mut self.inner.session,
            &carrier,
            tidepool_runtime::session::HostPayload::Json(value),
        )
    }

    fn observe(&mut self, source: &str) -> ResidentOutcome {
        self.checked.observe(&mut self.inner.session, source, "")
    }
}

#[test]
fn different_incarnations_sharing_one_root_do_not_cross_read_same_named_generations() {
    eval_harness::require_extract();
    let root = tempfile::tempdir().expect("shared session root");
    let mut first = CheckedIncarnation::open(tidepool_repr::SessionId(910_001), root.path());
    let first_binder = first.mount_json("carried", &serde_json::json!({"who": "first"}));
    let mut second = CheckedIncarnation::open(tidepool_repr::SessionId(910_002), root.path());
    let second_binder = second.mount_json("carried", &serde_json::json!({"who": "second"}));
    assert_eq!(
        first_binder.module, second_binder.module,
        "independent issuers exercise the same module name with different values"
    );
    assert_eq!(first_binder.name, second_binder.name);
    for _ in 0..2 {
        let outcome = second.observe(
            "case carried of { Aeson.Object o -> \
         if Map.lookup \"who\" o == Just (Aeson.String \"second\") \
         then () else error \"cross-incarnation payload mismatch\"; \
         _ -> error \"cross-incarnation constructor mismatch\" }",
        );
        assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
        let outcome = first.observe("case carried of { Aeson.Object o -> if Map.lookup \"who\" o == Just (Aeson.String \"first\") then () else error \"first incarnation payload mismatch\"; _ -> error \"first constructor mismatch\" }");
        assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
    }
}

/// Fifty distinct issued interfaces share one native JSON constructor prototype.
/// Two separately checked readbacks must retain every ordered accepted value.
#[test]
fn fifty_accumulated_session_bindings_read_back_correctly_across_repeat_compiles() {
    eval_harness::require_extract();
    let root = tempfile::tempdir().expect("session root");
    let mut incarnation = CheckedIncarnation::open(tidepool_repr::SessionId(920_001), root.path());

    const COUNT: usize = 50;
    let mut names = Vec::with_capacity(COUNT);
    for i in 0..COUNT {
        let name = format!("carried{i}");
        incarnation.mount_json(&name, &serde_json::json!({"tag": i}));
        names.push(name);
    }

    let read_back_expr = format!(
        "if [ v | Aeson.Object o <- [{}], Just v <- [Map.lookup \"tag\" o] ] \
         == map toJSON ([0..{}] :: [Int]) \
         then () else error \"accumulated carrier payload order mismatch\"",
        names.join(", "),
        COUNT - 1
    );

    for attempt in 0..2 {
        let outcome = incarnation.observe(&read_back_expr);
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "attempt {attempt}: all 50 ordered payload assertions must complete: {outcome:?}"
        );
    }
}

#[test]
fn closing_one_realm_notifies_exact_retirement_and_preserves_sibling() {
    use std::sync::{Arc, Mutex};
    use tidepool_codegen::suspension::RealmId;
    use tidepool_effect::{EffectRunPolicy, LivePayloadPolicy};
    use tidepool_runtime::session::{ResidentContinuationEvent, SessionRunContext};

    let mut notebook = Notebook::with_effects(&[tidepool_mcp::console_decl()]);
    let compiled = notebook.prepare_expression("say \"pause\" >> pure (42 :: Int)");
    let realms = [RealmId::fresh(), RealmId::fresh()];
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    let observer: Arc<dyn Fn(ResidentContinuationEvent) + Send + Sync> =
        Arc::new(move |event| observed.lock().unwrap().push(event));
    let mut holes = Vec::new();
    for realm in realms {
        notebook
            .session
            .set_actor_execution(
                SessionRunContext {
                    resource_scope: realm,
                    ..SessionRunContext::ROOT
                },
                EffectRunPolicy::SuspendAll,
                LivePayloadPolicy::HASKELL_EFFECT_VALUE,
            )
            .unwrap();
        let outcome = notebook
            .session
            .with_continuation_observer(observer.clone(), |session| {
                source_action(session, &notebook.source, |session, settlement| {
                    session.run_with_sites("realm_observation", compiled.code(), settlement)
                })
                .unwrap()
            });
        let ResidentOutcome::Suspended { hole, .. } = outcome else {
            panic!("actual Console frame must park");
        };
        holes.push(hole);
    }
    let original = events.lock().unwrap().clone();
    assert_eq!(
        original,
        holes
            .iter()
            .map(|hole| ResidentContinuationEvent::Parked(hole.cont_id().to_owned()))
            .collect::<Vec<_>>()
    );
    let counts = notebook
        .session
        .with_continuation_observer(observer.clone(), |session| session.close_realm(realms[0]));
    assert_eq!(counts.0, 1, "the closed realm retires its actual frame");
    assert_eq!(notebook.session.parked_holes(), [holes[1].cont_id()]);
    let mut expected = original;
    expected.push(ResidentContinuationEvent::Retired(
        holes[0].cont_id().to_owned(),
    ));
    assert_eq!(*events.lock().unwrap(), expected);
    assert_eq!(
        notebook
            .session
            .with_continuation_observer(observer.clone(), |session| session.close_realm(realms[0])),
        (0, 0)
    );
    assert_eq!(
        *events.lock().unwrap(),
        expected,
        "idempotent close reports no second retirement"
    );
    let outcome = notebook
        .session
        .with_continuation_observer(observer, |session| {
            tidepool_testing::with_settlement(|settlement| {
                session.resume_classified(holes[1].clone(), (), settlement)
            })
            .unwrap()
        });
    assert!(matches!(outcome, ResidentOutcome::Completed { .. }));
    expected.push(ResidentContinuationEvent::Retired(
        holes[1].cont_id().to_owned(),
    ));
    assert_eq!(*events.lock().unwrap(), expected);
    assert!(notebook.session.parked_holes().is_empty());
}
