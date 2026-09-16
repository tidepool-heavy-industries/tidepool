//! One notebook, two engines. The same turns -- an expression, a bind, and a
//! later expression that imports the bound value -- run through the
//! production owners (`run_turn` over the shared workbench templates, then
//! `ResidentSession`) on the Core route and on the prepared-STG route. The
//! prepared route compiles each turn with its prepared program, links later
//! turns against the session's live prepared bindings, and never runs Core.
//!
//! Needs a resolvable `$TIDEPOOL_EXTRACT` and its Haskell worker
//! (`just test-target tidepool-runtime session 'test(prepared_turn)'`).

use std::path::{Path, PathBuf};

use tidepool_repr::{
    execution_schema::{Group, TypeNode},
    Generation, SessionId,
};
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, BoundBinder, EngineKind, ModuleEnv,
    PreparedRuntimeError, ResidentError, ResidentOutcome, ResidentSession, SessionLib,
    SourceImports, TurnRequest, TurnResult, TurnTemplate, ValueTier,
};
use tidepool_testing::eval_harness;

/// A minimal notebook over one resident session: it tracks the value
/// generation and the bound value modules a later turn imports, exactly as
/// the actor workbench's compile view does.
struct Notebook {
    session: ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    preamble: String,
    effect_stack: String,
    include: Vec<PathBuf>,
    root: tempfile::TempDir,
    /// Bound value modules (`Tidepool.Session.Val.G<g>`): imported by every
    /// later turn's template and injected into its compile, as the actor
    /// workbench's compile view does.
    injected: Vec<String>,
    generation: u64,
    /// The session constructor table as of the last expression turn, for
    /// naming constructors in host-built answers.
    last_table: Option<tidepool_repr::DataConTable>,
}

impl Notebook {
    fn new(engine: EngineKind) -> Self {
        eval_harness::require_extract();
        let decls = tidepool_mcp::standard_decls();
        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let effect_stack = tidepool_mcp::build_effect_stack_type(&decls);
        let mut include = eval_harness::effects_include().to_vec();
        include.push(eval_harness::prelude_path());
        let root = tempfile::tempdir().expect("session root");
        // A decl plane, so a notebook can declare top-level functions
        // (`Notebook::declare`) alongside its value binds. Its gen modules
        // live under `root` at highest include precedence -- the same rule
        // `SessionLib::include_dir` documents -- so a later turn's `import
        // Tidepool.Session.Lib.G<g>` resolves.
        let lib = SessionLib::open(
            SessionId(1),
            root.path().join("decl-lib"),
            ModuleEnv::standalone_default(),
        )
        .expect("open decl plane")
        .with_validation_include(vec![eval_harness::prelude_path()]);
        include.push(lib.include_dir().to_path_buf());
        let session = ResidentSession::unbootstrapped_on(
            engine,
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            Some(lib),
        );
        Self {
            session,
            preamble,
            effect_stack,
            include,
            root,
            injected: Vec::new(),
            generation: 0,
            last_table: None,
        }
    }

    /// Define a top-level decl (e.g. `addN :: Int -> Int -> Int; addN n x =
    /// x + n`) on the decl plane's ROOT scope; a later turn sees it because
    /// its `Lib.G<g>` module is pushed onto `injected`, exactly as `bind`
    /// pushes a bound value's module.
    fn declare(&mut self, source: &str) -> Generation {
        let generation = self
            .session
            .define_scoped_with_imports_in(
                tidepool_codegen::scope::ScopeId::ROOT,
                &[source],
                &SourceImports::new(),
            )
            .unwrap_or_else(|error| panic!("{source:?} failed to declare: {error}"));
        let module = self
            .session
            .session_import_module_in(tidepool_codegen::scope::ScopeId::ROOT)
            .expect("declare committed a generation but no lib module is visible");
        self.injected.push(module);
        generation
    }

    /// A constructor's bridge id from the session table the last expression
    /// turn left behind.
    fn constructor(&self, name: &str) -> tidepool_repr::DataConId {
        self.last_table
            .as_ref()
            .expect("an expression turn ran")
            .get_by_name(name)
            .unwrap_or_else(|| panic!("constructor {name} is not in the session table"))
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
        run_turn(TurnRequest {
            turn_text: text,
            templates: &templates,
            include: &include,
            session_root: self.root.path(),
            inject_modules: &self.injected,
            gen: self.generation,
            verdict: None,
            target: None,
            prepared: self.session.prepared_turn_request(&retained),
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

    /// Run an expression turn and render its result as JSON.
    fn expression(&mut self, text: &str) -> serde_json::Value {
        let TurnResult::Expr { compiled, .. } = self.compile(text) else {
            panic!("{text:?} did not classify as an expression");
        };
        let outcome = self
            .session
            .run_with_sites("notebook_expression", compiled.code())
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        let ResidentOutcome::Completed { result, .. } = outcome else {
            panic!("{text:?} did not complete: {outcome:?}");
        };
        self.last_table = Some(result.table().clone());
        tidepool_runtime::value_to_json(result.value(), result.table(), 0)
    }

    /// Run a single-binder ask turn `<binder_name> <- <expr>` to its
    /// suspension and return the binder and hole, checking that the request
    /// names one of the turn's declared sites and, on the prepared route,
    /// that the artifact admits the resume entry.
    fn suspend_ask(
        &mut self,
        engine: EngineKind,
        binder_name: &str,
        expr: &str,
    ) -> (BoundBinder, tidepool_runtime::session::ResidentHole) {
        let TurnResult::Bind {
            bound, compiled, ..
        } = self.compile(&format!("{binder_name} <- {expr}"))
        else {
            panic!("{engine:?}: the {binder_name} ask did not classify as a bind");
        };
        let [binder] = bound.as_slice() else {
            panic!(
                "{engine:?}: the {binder_name} ask bound {} names",
                bound.len()
            );
        };
        assert_eq!(binder.name, binder_name);
        let code = compiled.code();
        let declared_sites: Vec<u64> = match engine {
            EngineKind::Prepared => {
                let prepared = compiled
                    .prepared
                    .as_ref()
                    .expect("prepared request returned no prepared program");
                // The artifact admits the resume entry beside the settled
                // scaffold: the host checks the artifact, not the template text.
                let admits_resume = prepared
                    .bindings()
                    .iter()
                    .flat_map(|group| match group {
                        Group::NonRecursive(top) => std::slice::from_ref(top),
                        Group::Recursive(tops) => tops.as_slice(),
                    })
                    .any(|top| top.identity.occurrence == "__resume");
                assert!(admits_resume, "the turn artifact admits no __resume top");
                prepared.sites().iter().map(|row| row.site).collect()
            }
            EngineKind::Core => code.sites.iter().map(|site| site.site).collect(),
        };
        assert!(
            !declared_sites.is_empty(),
            "{engine:?}: the {binder_name} ask declares no site"
        );
        let outcome = self
            .session
            .run_bind_with_sites(
                "notebook_ask",
                compiled.code(),
                binder,
                Generation(self.generation),
            )
            .unwrap_or_else(|error| {
                panic!("{engine:?}: the {binder_name} ask failed to run: {error}")
            });
        let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
            panic!("{engine:?}: the {binder_name} ask did not suspend: {outcome:?}");
        };
        let request = tidepool_runtime::value_to_json(&request, code.table, 0);
        let site = typed_site_of(&request).unwrap_or_else(|| {
            panic!("{engine:?}: the {binder_name} request names no typedSite: {request}")
        });
        assert!(
            declared_sites.contains(&site),
            "{engine:?}: site {site} is not one of the turn's {declared_sites:?}"
        );
        (binder.clone(), hole)
    }

    /// Snapshot the value-handle and persistent-root counts, feed `answer` to
    /// `hole`, and assert the resume is refused with `expected`: the frame
    /// stays parked (`hole` is still among the `parked` parked holes, and
    /// `parked_count`/`stowed_roots_count` read `parked`), and nothing else
    /// moved -- `value_handle_count` and `persistent_roots_count` return to
    /// what they were before the probe.
    fn assert_refusal_leaves_frame_parked(
        &mut self,
        hole: &tidepool_runtime::session::ResidentHole,
        answer: tidepool_bridge::Value,
        expected: fn(&ResidentError) -> bool,
        parked: usize,
    ) {
        let handles_parked = self.session.value_handle_count();
        let roots_parked = self.session.persistent_roots_count();
        let refused = self
            .session
            .resume(hole.clone(), answer)
            .expect_err("a bad answer must be refused");
        assert!(expected(&refused), "unexpected refusal: {refused}");
        assert!(
            self.session.parked_holes().contains(&hole.cont_id()),
            "the rejected answer disturbed the parked set"
        );
        assert_eq!(self.session.parked_count(), parked);
        assert_eq!(self.session.stowed_roots_count(), parked);
        assert_eq!(self.session.value_handle_count(), handles_parked);
        assert_eq!(self.session.persistent_roots_count(), roots_parked);
    }

    /// Resume `hole` with `answer`, assert the bind completed, and -- as
    /// every successful ask does next -- push `binder`'s module onto
    /// `injected` so a later turn can import it.
    fn resume_bind(
        &mut self,
        hole: tidepool_runtime::session::ResidentHole,
        binder: &BoundBinder,
        answer: tidepool_bridge::Value,
    ) -> ResidentOutcome {
        let outcome = self
            .session
            .resume(hole, answer)
            .unwrap_or_else(|error| panic!("resuming {} failed: {error}", binder.name));
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "the resumed {} bind did not complete: {outcome:?}",
            binder.name
        );
        self.injected.push(binder.module.clone());
        outcome
    }

    /// Run a single-binder bind turn; later turns see the binder's module.
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
        let outcome = self
            .session
            .run_bind_with_sites(
                "notebook_bind",
                compiled.code(),
                binder,
                Generation(self.generation),
            )
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "{text:?} did not complete: {outcome:?}"
        );
        self.injected.push(binder.module.clone());
        binder.clone()
    }

    /// Run a pattern bind (`(x, y) <- ...`): every GHC binder is bound from
    /// the projected tuple in one atomic step.
    fn bind_pattern(&mut self, text: &str) -> Vec<BoundBinder> {
        let TurnResult::Bind {
            bound, compiled, ..
        } = self.compile(text)
        else {
            panic!("{text:?} did not classify as a bind");
        };
        assert!(bound.len() > 1, "{text:?} bound {} names", bound.len());
        let outcome = self
            .session
            .run_projected_bind_with_sites(
                "notebook_pattern_bind",
                compiled.code(),
                &bound,
                Generation(self.generation),
            )
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        assert!(
            matches!(outcome, ResidentOutcome::BindingsCommitted { .. }),
            "{text:?} did not commit its bindings: {outcome:?}"
        );
        self.injected.push(bound[0].module.clone());
        bound
    }
}

fn notebook_turns(engine: EngineKind) {
    let mut notebook = Notebook::new(engine);
    assert_eq!(notebook.session.engine_kind(), engine);

    let rendered = notebook.expression("41 + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: 41 + 1 rendered as {rendered}"
    );

    let binder = notebook.bind("x <- pure (20 :: Int)");
    assert_eq!(binder.name, "x");
    let (_, module, tier, _) = notebook
        .session
        .current_binding_in(tidepool_codegen::scope::ScopeId::ROOT, "x")
        .expect("x is bound");
    assert_eq!(module.module_name(), binder.module);
    // A prepared value is tenured as-is (Tier-1's preparation policy); the
    // Core route deep-forces first-order data to Tier-0. The tier is how the
    // value plane reports which engine bound `x`.
    let expected_tier = match engine {
        EngineKind::Prepared => ValueTier::Tier1Closure,
        EngineKind::Core => ValueTier::Tier0Data,
    };
    assert_eq!(
        tier, expected_tier,
        "{engine:?}: x was bound with the wrong preparation"
    );

    // A later turn imports the bound value: on the prepared route it links
    // against the binding by identity and generation instead of recompiling
    // a body it does not have.
    let rendered = notebook.expression("x + 22").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: x + 22 rendered as {rendered}"
    );

    // A pattern bind: the extractor projects the binders as one tuple and
    // the session binds its fields, each importable by its own name.
    let bound = notebook.bind_pattern("(lo, hi) <- pure (x - 19, x + 80)");
    let names: Vec<&str> = bound.iter().map(|binder| binder.name.as_str()).collect();
    assert_eq!(names, ["lo", "hi"], "{engine:?}");
    for name in ["lo", "hi"] {
        assert!(
            notebook
                .session
                .current_binding_in(tidepool_codegen::scope::ScopeId::ROOT, name)
                .is_some(),
            "{engine:?}: {name} is not bound"
        );
    }
    let rendered = notebook.expression("hi - lo").to_string();
    assert!(
        rendered.contains("99"),
        "{engine:?}: hi - lo rendered as {rendered}"
    );
}

#[test]
fn notebook_turns_run_on_core() {
    notebook_turns(EngineKind::Core);
}

#[test]
fn notebook_turns_run_on_prepared_stg() {
    notebook_turns(EngineKind::Prepared);
}

/// The `typedSite` a suspended request names, read from its rendered JSON the
/// way the harness classifies a suspension.
fn typed_site_of(json: &serde_json::Value) -> Option<u64> {
    match json {
        serde_json::Value::Object(object) => object
            .get("typedSite")
            .and_then(serde_json::Value::as_u64)
            .or_else(|| object.values().find_map(typed_site_of)),
        serde_json::Value::Array(items) => items.iter().find_map(typed_site_of),
        _ => None,
    }
}

/// A typed effect request parks the turn on either engine: the suspension
/// reports the request naming one of the turn's declared sites, the frame is
/// rooted while an unrelated turn runs, a host-built `True` completes the
/// bind on both routes (the prepared route validates the answer against the
/// site's type evidence first, refusing a literal and a wrong-family
/// constructor with the frame intact), and `abort` retires a second ask's
/// hole with the same error on both routes, leaving the session's resource
/// counts where the turn found them.
fn notebook_suspension(engine: EngineKind) {
    use tidepool_bridge::Value;
    use tidepool_repr::Literal;

    let mut notebook = Notebook::new(engine);
    let rendered = notebook.expression("41 + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: warm-up rendered {rendered}"
    );
    // Brings `True`/`False` into the session table beside `I#`.
    let rendered = notebook.expression("not False").to_string();
    assert!(
        rendered.contains("true"),
        "{engine:?}: not False rendered as {rendered}"
    );
    let true_id = notebook.constructor("True");
    let boxed_int = notebook.constructor("I#");
    let handles_before = notebook.session.value_handle_count();
    assert_eq!(notebook.session.parked_count(), 0);

    let (binder, hole) = notebook.suspend_ask(engine, "b", "(runLLMTurn @Bool \"q\" :: M Bool)");
    assert_eq!(notebook.session.parked_holes(), vec![hole.cont_id()]);
    assert_eq!(notebook.session.parked_count(), 1);
    assert_eq!(notebook.session.stowed_roots_count(), 1);

    // The frame stays parked and rooted while an unrelated turn runs.
    let rendered = notebook.expression("40 + 2").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: 40 + 2 rendered as {rendered}"
    );
    assert_eq!(notebook.session.parked_count(), 1);
    assert_eq!(notebook.session.stowed_roots_count(), 1);

    if engine == EngineKind::Prepared {
        // The validator refuses an answer that does not fit the site's type
        // evidence before the frame is touched: a bare literal for a Bool
        // site, and a constructor from another family. Core has no validator
        // of its own (the harness checks answers upstream), so these probes
        // are the prepared route's.
        notebook.assert_refusal_leaves_frame_parked(
            &hole,
            Value::Lit(Literal::LitInt(1)),
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerShape { .. })
                )
            },
            1,
        );
        notebook.assert_refusal_leaves_frame_parked(
            &hole,
            Value::Con(boxed_int, vec![Value::Lit(Literal::LitInt(1))]),
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerConstructor { .. })
                )
            },
            1,
        );
    }

    // The host-built answer completes the bind through the frame's resume
    // entry; the bound value is the answer.
    notebook.resume_bind(hole, &binder, Value::Con(true_id, Vec::new()));
    assert!(notebook.session.parked_holes().is_empty(), "{engine:?}");
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");
    // A prepared binding keeps its value as a ROOT-realm ledger handle
    // (`BoundValue::Prepared`); Core moves the tenured root out of the handle
    // registry into the binding table. Either way, nothing else is left.
    let bound_handles = match engine {
        EngineKind::Prepared => 1,
        EngineKind::Core => 0,
    };
    assert_eq!(
        notebook.session.value_handle_count(),
        handles_before + bound_handles,
        "{engine:?}: the resumed turn leaked a value handle"
    );
    let rendered = notebook.expression("not b").to_string();
    assert!(
        rendered.contains("false"),
        "{engine:?}: not b rendered as {rendered}"
    );

    // A second ask, this time aborted.
    let handles_before = notebook.session.value_handle_count();
    let roots_before = notebook.session.persistent_roots_count();
    let (_, hole) = notebook.suspend_ask(engine, "b", "(runLLMTurn @Bool \"q\" :: M Bool)");

    // On the prepared route every installed program's heap tops stay rooted
    // until program retirement lands (the residency wave), so the abort is
    // measured against the roots present just before it: it must release
    // exactly what the frame owned and nothing else. Core, which retires no
    // programs, returns to the pre-turn count.
    let roots_before_abort = notebook.session.persistent_roots_count();
    let aborted = notebook
        .session
        .abort(hole.cont_id(), "test abort".into())
        .expect_err("abort fails the ask");
    assert!(
        aborted
            .to_string()
            .contains("ask aborted by caller: test abort"),
        "{engine:?}: abort reported {aborted}"
    );
    assert!(notebook.session.parked_holes().is_empty(), "{engine:?}");
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");
    assert_eq!(
        notebook.session.value_handle_count(),
        handles_before,
        "{engine:?}: the aborted turn leaked a value handle"
    );
    let expected_roots = match engine {
        // The frame's live payload root is released with the frame.
        EngineKind::Core => {
            assert_eq!(
                roots_before_abort,
                roots_before + 1,
                "Core parks one payload root"
            );
            roots_before
        }
        // The parked continuation's slot returns to the persistent list at
        // take and is released with the handle: net zero.
        EngineKind::Prepared => roots_before_abort,
    };
    assert_eq!(
        notebook.session.persistent_roots_count(),
        expected_roots,
        "{engine:?}: the aborted turn leaked a persistent root"
    );

    // The session stays usable after the abort.
    let rendered = notebook.expression("40 + 2").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: 40 + 2 rendered as {rendered}"
    );
}

#[test]
fn notebook_ask_parks_and_aborts_on_core() {
    notebook_suspension(EngineKind::Core);
}

#[test]
fn notebook_ask_parks_and_aborts_on_prepared_stg() {
    notebook_suspension(EngineKind::Prepared);
}

#[test]
fn prepared_turn_includes_the_complete_site_answer_family_in_shared_metadata() {
    let mut notebook = Notebook::new(EngineKind::Prepared);
    notebook.preamble.push('\n');
    notebook.preamble.push_str(include_str!(
        "fixtures/prepared/SiteOnlyConstructorCoverage.hs"
    ));

    let TurnResult::Expr { compiled, .. } = notebook.compile("siteOnlyConstructorCoverage") else {
        panic!("site-only constructor coverage helper did not classify as an expression");
    };
    let prepared = compiled
        .prepared
        .as_ref()
        .expect("prepared request returned no prepared program");

    let site = prepared
        .sites()
        .iter()
        .find(|site| {
            matches!(
                prepared.type_node(site.wire),
                Some(TypeNode::Data { family, .. })
                    if family.occurrence == "SiteOnlyAnswer"
            )
        })
        .expect("runLLMTurn site has no SiteOnlyAnswer root");
    let TypeNode::Data { family, rows, .. } = prepared
        .type_node(site.wire)
        .expect("validated site root must resolve")
    else {
        unreachable!("the selected site root is known to be Data");
    };
    assert_eq!(family.occurrence, "SiteOnlyAnswer");

    let constructor_names: Vec<_> = rows
        .iter()
        .map(|row| {
            prepared.constructors()[row.constructor.0 as usize]
                .identity
                .occurrence
                .as_str()
        })
        .collect();
    assert_eq!(
        constructor_names,
        ["SiteOnlyChosen", "SiteOnlyNeverMatched"],
        "the site root must carry the complete answer family"
    );

    let missing: Vec<_> = prepared
        .constructors()
        .iter()
        .filter(|constructor| compiled.table.get(constructor.host_id).is_none())
        .map(|constructor| constructor.identity.clone())
        .collect();
    assert!(
        missing.is_empty(),
        "prepared constructors absent from the shared DataConTable: {missing:?}"
    );
}

/// Two host-built answers that are constructors with fields -- a boxed `Int`
/// (`I#`) and a `Maybe Int` (`Just`/`Nothing`) -- resuming parked prepared
/// turns on both engines. This extends `notebook_suspension`'s `Bool`
/// coverage (a nullary constructor) to constructors that carry scalar and
/// nested-constructor fields.
///
/// On the prepared route the validator refuses a bare literal for a `Data`
/// site (`AnswerShape`: a constructor is required, not a scalar directly)
/// and a constructor from another family (`AnswerConstructor`) before the
/// frame is touched, exactly as `notebook_suspension` documents for `Bool`.
/// Core has no validator of its own -- the harness validates answers
/// upstream -- so these probes are prepared-only.
fn notebook_data_answers(engine: EngineKind) {
    use tidepool_bridge::Value;
    use tidepool_repr::Literal;

    let mut notebook = Notebook::new(engine);
    let rendered = notebook.expression("41 + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: warm-up rendered {rendered}"
    );
    // Brings `I#` into the session table.
    let i_hash_id = notebook.constructor("I#");
    // Brings `True`/`False` into the session table beside `I#`.
    let rendered = notebook.expression("not False").to_string();
    assert!(
        rendered.contains("true"),
        "{engine:?}: not False rendered as {rendered}"
    );
    let true_id = notebook.constructor("True");
    // Brings `Just`/`Nothing` into the session table. `Just` alone is
    // ambiguous by bare name in this table (more than one constructor
    // shares it), so resolve it by arity instead of `Notebook::constructor`.
    let rendered = notebook
        .expression("maybe (0 :: Int) (+ 1) (Just (2 :: Int))")
        .to_string();
    assert!(
        rendered.contains('3'),
        "{engine:?}: warm-up maybe rendered as {rendered}"
    );
    let just_id = notebook
        .last_table
        .as_ref()
        .expect("an expression turn ran")
        .get_by_name_arity("Just", 1)
        .unwrap_or_else(|| panic!("{engine:?}: Just/1 is not in the session table"));

    assert_eq!(notebook.session.parked_count(), 0);

    // --- n <- runLLMTurn @Int: a `Data` site whose one row (`I#`) carries a
    // scalar field. ---
    let handles_before = notebook.session.value_handle_count();
    let (n_binder, n_hole) =
        notebook.suspend_ask(engine, "n", "(runLLMTurn @Int \"how many\" :: M Int)");
    assert_eq!(notebook.session.parked_count(), 1);
    assert_eq!(notebook.session.stowed_roots_count(), 1);

    if engine == EngineKind::Prepared {
        notebook.assert_refusal_leaves_frame_parked(
            &n_hole,
            Value::Lit(Literal::LitInt(7)),
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerShape { .. })
                )
            },
            1,
        );
        notebook.assert_refusal_leaves_frame_parked(
            &n_hole,
            Value::Con(true_id, Vec::new()),
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerConstructor { .. })
                )
            },
            1,
        );
    }

    notebook.resume_bind(
        n_hole.clone(),
        &n_binder,
        Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(41))]),
    );
    assert!(notebook.session.parked_holes().is_empty(), "{engine:?}");
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");
    // A prepared binding keeps its value as a ROOT-realm ledger handle
    // (`BoundValue::Prepared`); Core moves the tenured root out of the handle
    // registry into the binding table.
    let n_bound_handles = match engine {
        EngineKind::Prepared => 1,
        EngineKind::Core => 0,
    };
    assert_eq!(
        notebook.session.value_handle_count(),
        handles_before + n_bound_handles,
        "{engine:?}: the resumed n turn leaked a value handle"
    );
    let second = notebook
        .session
        .resume(
            n_hole.clone(),
            Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(0))]),
        )
        .expect_err("the n hole is already settled");
    assert!(
        matches!(second, ResidentError::WrongContinuation { .. }),
        "{engine:?}: a second resume of the settled n hole reported {second}"
    );

    let rendered = notebook.expression("n + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: n + 1 rendered as {rendered}"
    );

    // --- m <- runLLMTurn @(Maybe Int): a `Data` site whose rows are
    // `Just`/`Nothing`, `Just` carrying a nested `I#` field. ---
    let handles_before = notebook.session.value_handle_count();
    let (m_binder, m_hole) = notebook.suspend_ask(
        engine,
        "m",
        "(runLLMTurn @(Maybe Int) \"maybe\" :: M (Maybe Int))",
    );
    assert_eq!(notebook.session.parked_count(), 1);
    assert_eq!(notebook.session.stowed_roots_count(), 1);

    if engine == EngineKind::Prepared {
        let wrong_family = notebook
            .session
            .resume(
                m_hole.clone(),
                Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(1))]),
            )
            .expect_err("an I# constructor is not a Maybe Int");
        assert!(
            matches!(
                wrong_family,
                ResidentError::Prepared(PreparedRuntimeError::AnswerConstructor { .. })
            ),
            "unexpected refusal: {wrong_family}"
        );
        assert_eq!(notebook.session.parked_holes(), vec![m_hole.cont_id()]);
        assert_eq!(notebook.session.parked_count(), 1);
        assert_eq!(notebook.session.stowed_roots_count(), 1);
    }

    notebook.resume_bind(
        m_hole.clone(),
        &m_binder,
        Value::Con(
            just_id,
            vec![Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(4))])],
        ),
    );
    assert!(notebook.session.parked_holes().is_empty(), "{engine:?}");
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");
    let m_bound_handles = match engine {
        EngineKind::Prepared => 1,
        EngineKind::Core => 0,
    };
    assert_eq!(
        notebook.session.value_handle_count(),
        handles_before + m_bound_handles,
        "{engine:?}: the resumed m turn leaked a value handle"
    );
    let second = notebook
        .session
        .resume(
            m_hole.clone(),
            Value::Con(
                just_id,
                vec![Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(0))])],
            ),
        )
        .expect_err("the m hole is already settled");
    assert!(
        matches!(second, ResidentError::WrongContinuation { .. }),
        "{engine:?}: a second resume of the settled m hole reported {second}"
    );

    let rendered = notebook.expression("maybe (0 :: Int) (+ 1) m").to_string();
    assert!(
        rendered.contains('5'),
        "{engine:?}: maybe 0 (+ 1) m rendered as {rendered}"
    );
}

#[test]
fn notebook_data_answers_on_core() {
    notebook_data_answers(EngineKind::Core);
}

#[test]
fn notebook_data_answers_on_prepared_stg() {
    notebook_data_answers(EngineKind::Prepared);
}

/// A host answer aimed at a continuation that is no longer parked -- because
/// it already completed, or because it was aborted -- is the session's
/// shared `WrongContinuation` bookkeeping error on either engine (the same
/// validate-before-consume path `reenter` uses for `resume` and `abort`
/// alike, already exercised once in `notebook_data_answers` for a completed
/// bind without checking counts). Neither rejection touches the parked set,
/// the stowed roots, the value handles or the persistent roots, and the
/// session is not latched: a following valid ask still resolves normally.
fn notebook_resume_after_settle(engine: EngineKind) {
    use tidepool_bridge::Value;

    let mut notebook = Notebook::new(engine);
    let rendered = notebook.expression("41 + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: warm-up rendered {rendered}"
    );
    // Brings `True`/`False` into the session table.
    let rendered = notebook.expression("not False").to_string();
    assert!(
        rendered.contains("true"),
        "{engine:?}: not False rendered as {rendered}"
    );
    let true_id = notebook.constructor("True");
    assert_eq!(notebook.session.parked_count(), 0);

    // --- A second answer to an id that already completed. ---
    let (binder, hole) = notebook.suspend_ask(engine, "b", "(runLLMTurn @Bool \"q\" :: M Bool)");
    notebook.resume_bind(hole.clone(), &binder, Value::Con(true_id, Vec::new()));
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");

    let handles_settled = notebook.session.value_handle_count();
    let roots_settled = notebook.session.persistent_roots_count();
    let second = notebook
        .session
        .resume(hole.clone(), Value::Con(true_id, Vec::new()))
        .expect_err("the hole already completed");
    assert!(
        matches!(
            &second,
            ResidentError::WrongContinuation { attempted, pending }
                if attempted == hole.cont_id() && pending.is_empty()
        ),
        "{engine:?}: a second resume of the completed hole reported {second}"
    );
    assert_eq!(
        notebook.session.parked_count(),
        0,
        "{engine:?}: the second resume changed parked_count"
    );
    assert_eq!(
        notebook.session.stowed_roots_count(),
        0,
        "{engine:?}: the second resume changed stowed_roots_count"
    );
    assert_eq!(
        notebook.session.value_handle_count(),
        handles_settled,
        "{engine:?}: the second resume changed value_handle_count"
    );
    assert_eq!(
        notebook.session.persistent_roots_count(),
        roots_settled,
        "{engine:?}: the second resume changed persistent_roots_count"
    );

    // The session is not latched: a following valid ask still resolves.
    let rendered = notebook.expression("not b").to_string();
    assert!(
        rendered.contains("false"),
        "{engine:?}: not b rendered as {rendered}"
    );

    // --- An answer to an id that was aborted. ---
    let (_, hole) = notebook.suspend_ask(engine, "b", "(runLLMTurn @Bool \"q\" :: M Bool)");
    let cont_id = hole.cont_id().to_string();
    let aborted = notebook
        .session
        .abort(&cont_id, "test abort".into())
        .expect_err("abort fails the ask");
    assert!(
        aborted
            .to_string()
            .contains("ask aborted by caller: test abort"),
        "{engine:?}: abort reported {aborted}"
    );
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");

    let handles_aborted = notebook.session.value_handle_count();
    let roots_aborted = notebook.session.persistent_roots_count();
    let after_abort = notebook
        .session
        .resume(hole, Value::Con(true_id, Vec::new()))
        .expect_err("the aborted hole is no longer parked");
    assert!(
        matches!(
            &after_abort,
            ResidentError::WrongContinuation { attempted, pending }
                if attempted == &cont_id && pending.is_empty()
        ),
        "{engine:?}: resuming the aborted hole reported {after_abort}"
    );
    assert_eq!(
        notebook.session.parked_count(),
        0,
        "{engine:?}: the post-abort resume changed parked_count"
    );
    assert_eq!(
        notebook.session.stowed_roots_count(),
        0,
        "{engine:?}: the post-abort resume changed stowed_roots_count"
    );
    assert_eq!(
        notebook.session.value_handle_count(),
        handles_aborted,
        "{engine:?}: the post-abort resume changed value_handle_count"
    );
    assert_eq!(
        notebook.session.persistent_roots_count(),
        roots_aborted,
        "{engine:?}: the post-abort resume changed persistent_roots_count"
    );

    // The session stays usable after both rejections.
    let rendered = notebook.expression("40 + 2").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: 40 + 2 rendered as {rendered}"
    );
}

#[test]
fn notebook_resume_after_settle_on_core() {
    notebook_resume_after_settle(EngineKind::Core);
}

#[test]
fn notebook_resume_after_settle_on_prepared_stg() {
    notebook_resume_after_settle(EngineKind::Prepared);
}

/// Three more shapes of host-answer rejection the prepared validator refuses
/// before touching the frame, extending `notebook_suspension` (a Bool site
/// rejecting an `I#`) and `notebook_data_answers` (`Int`/`Maybe Int` sites
/// rejecting a `Bool`): a Bool site rejecting a constructor from a family
/// with fields (`Just`, not the nullary `I#`), an arity mismatch on a
/// same-family constructor, and a nested `Maybe Bool` payload whose `Just`
/// field is the wrong family. Core has no validator of its own -- the harness
/// validates answers upstream -- so every rejection probe below is
/// prepared-only, exactly as the two tests it extends already gate theirs.
/// Each rejection is followed by the valid answer completing normally.
fn notebook_answer_shape_rejections(engine: EngineKind) {
    use tidepool_bridge::Value;
    use tidepool_repr::Literal;

    let mut notebook = Notebook::new(engine);
    let rendered = notebook.expression("41 + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: warm-up rendered {rendered}"
    );
    // Brings `I#` into the session table.
    let i_hash_id = notebook.constructor("I#");
    // Brings `True`/`False` into the session table beside `I#`.
    let rendered = notebook.expression("not False").to_string();
    assert!(
        rendered.contains("true"),
        "{engine:?}: not False rendered as {rendered}"
    );
    let true_id = notebook.constructor("True");
    // Brings `Just`/`Nothing` into the session table. `Just` alone is
    // ambiguous by bare name (more than one constructor shares it), so
    // resolve it by arity instead of `Notebook::constructor`.
    let rendered = notebook
        .expression("maybe (0 :: Int) (+ 1) (Just (2 :: Int))")
        .to_string();
    assert!(
        rendered.contains('3'),
        "{engine:?}: warm-up maybe rendered as {rendered}"
    );
    let just_id = notebook
        .last_table
        .as_ref()
        .expect("an expression turn ran")
        .get_by_name_arity("Just", 1)
        .unwrap_or_else(|| panic!("{engine:?}: Just/1 is not in the session table"));

    assert_eq!(notebook.session.parked_count(), 0);

    // --- b <- runLLMTurn @Bool: rejects `Just True`, a constructor from a
    // different family that (unlike `I#`) carries a nested constructor
    // field rather than a scalar. ---
    let (b_binder, b_hole) =
        notebook.suspend_ask(engine, "b", "(runLLMTurn @Bool \"q\" :: M Bool)");
    assert_eq!(notebook.session.parked_count(), 1);
    assert_eq!(notebook.session.stowed_roots_count(), 1);

    if engine == EngineKind::Prepared {
        notebook.assert_refusal_leaves_frame_parked(
            &b_hole,
            Value::Con(just_id, vec![Value::Con(true_id, Vec::new())]),
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerConstructor { .. })
                )
            },
            1,
        );
    }

    notebook.resume_bind(b_hole, &b_binder, Value::Con(true_id, Vec::new()));
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");
    let rendered = notebook.expression("not b").to_string();
    assert!(
        rendered.contains("false"),
        "{engine:?}: not b rendered as {rendered}"
    );

    // --- n <- runLLMTurn @Int: rejects `I#` applied to two fields instead of
    // one -- the right family, the wrong arity. ---
    let (n_binder, n_hole) =
        notebook.suspend_ask(engine, "n", "(runLLMTurn @Int \"how many\" :: M Int)");
    assert_eq!(notebook.session.parked_count(), 1);
    assert_eq!(notebook.session.stowed_roots_count(), 1);

    if engine == EngineKind::Prepared {
        notebook.assert_refusal_leaves_frame_parked(
            &n_hole,
            Value::Con(
                i_hash_id,
                vec![
                    Value::Lit(Literal::LitInt(1)),
                    Value::Lit(Literal::LitInt(2)),
                ],
            ),
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerShape { .. })
                )
            },
            1,
        );
    }

    notebook.resume_bind(
        n_hole,
        &n_binder,
        Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(41))]),
    );
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");
    let rendered = notebook.expression("n + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: n + 1 rendered as {rendered}"
    );

    // --- mb <- runLLMTurn @(Maybe Bool): rejects `Just (I# 1)` -- the outer
    // constructor is right, but its field is the wrong family for the site's
    // nested `Bool` node -- then accepts `Just True`. ---
    let (mb_binder, mb_hole) = notebook.suspend_ask(
        engine,
        "mb",
        "(runLLMTurn @(Maybe Bool) \"maybe bool\" :: M (Maybe Bool))",
    );
    assert_eq!(notebook.session.parked_count(), 1);
    assert_eq!(notebook.session.stowed_roots_count(), 1);

    if engine == EngineKind::Prepared {
        notebook.assert_refusal_leaves_frame_parked(
            &mb_hole,
            Value::Con(
                just_id,
                vec![Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(1))])],
            ),
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerConstructor { .. })
                )
            },
            1,
        );
    }

    notebook.resume_bind(
        mb_hole,
        &mb_binder,
        Value::Con(just_id, vec![Value::Con(true_id, Vec::new())]),
    );
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");
    let rendered = notebook.expression("maybe False id mb").to_string();
    assert!(
        rendered.contains("true"),
        "{engine:?}: maybe False id mb rendered as {rendered}"
    );
}

#[test]
fn notebook_answer_shape_rejections_on_core() {
    notebook_answer_shape_rejections(EngineKind::Core);
}

#[test]
fn notebook_answer_shape_rejections_on_prepared_stg() {
    notebook_answer_shape_rejections(EngineKind::Prepared);
}

/// Two typed asks suspend in the same session before either is answered. A
/// wrong-family answer to the first frame is refused with both frames
/// intact; the second is then answered validly, then the first -- completion
/// order follows answer order (not park order), and each hole's binder
/// resolves to its own answer, not the other's.
fn notebook_interleaved_parked_continuations(engine: EngineKind) {
    use tidepool_bridge::Value;
    use tidepool_repr::Literal;

    let mut notebook = Notebook::new(engine);
    let rendered = notebook.expression("41 + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: warm-up rendered {rendered}"
    );
    // Brings `I#` into the session table.
    let i_hash_id = notebook.constructor("I#");
    // Brings `True`/`False` into the session table beside `I#`.
    let rendered = notebook.expression("not False").to_string();
    assert!(
        rendered.contains("true"),
        "{engine:?}: not False rendered as {rendered}"
    );
    let true_id = notebook.constructor("True");
    assert_eq!(notebook.session.parked_count(), 0);

    let (p_binder, p_hole) =
        notebook.suspend_ask(engine, "p", "(runLLMTurn @Bool \"first\" :: M Bool)");
    assert_eq!(notebook.session.parked_count(), 1);
    let (q_binder, q_hole) =
        notebook.suspend_ask(engine, "q", "(runLLMTurn @Int \"second\" :: M Int)");
    assert_eq!(notebook.session.parked_count(), 2);
    assert_eq!(notebook.session.stowed_roots_count(), 2);
    let mut expected_holes = vec![p_hole.cont_id(), q_hole.cont_id()];
    expected_holes.sort_unstable();
    let mut holes = notebook.session.parked_holes();
    holes.sort_unstable();
    assert_eq!(holes, expected_holes, "{engine:?}: both frames are parked");

    if engine == EngineKind::Prepared {
        // Core has no answer validator of its own; the wrong-family rejection
        // itself is exercised elsewhere (`notebook_suspension`,
        // `notebook_answer_shape_rejections`). Here it is only the probe that
        // the SECOND parked frame is undisturbed by a rejected answer to the
        // first.
        notebook.assert_refusal_leaves_frame_parked(
            &p_hole,
            Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(1))]),
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerConstructor { .. })
                )
            },
            2,
        );
        // The generic refusal invariant only pins `p_hole` as still parked;
        // here the SECOND frame (`q_hole`) must also be undisturbed.
        let mut holes = notebook.session.parked_holes();
        holes.sort_unstable();
        assert_eq!(
            holes, expected_holes,
            "{engine:?}: the rejection disturbed the parked set"
        );
    }

    // Answer the SECOND frame first: completion order follows answer order,
    // not park order.
    notebook.resume_bind(
        q_hole.clone(),
        &q_binder,
        Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(7))]),
    );
    assert_eq!(notebook.session.parked_count(), 1, "{engine:?}");
    assert_eq!(
        notebook.session.parked_holes(),
        vec![p_hole.cont_id()],
        "{engine:?}: only the first frame remains parked"
    );

    // Then the first.
    notebook.resume_bind(p_hole.clone(), &p_binder, Value::Con(true_id, Vec::new()));
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");

    // Each binder resolves to its own answer, not the other's.
    let rendered = notebook.expression("(p, q)").to_string();
    assert!(
        rendered.contains("true") && rendered.contains('7'),
        "{engine:?}: (p, q) rendered as {rendered}"
    );
    let rendered = notebook.expression("not p").to_string();
    assert!(
        rendered.contains("false"),
        "{engine:?}: not p rendered as {rendered}"
    );
    let rendered = notebook.expression("q + 1").to_string();
    assert!(
        rendered.contains('8'),
        "{engine:?}: q + 1 rendered as {rendered}"
    );
}

#[test]
fn notebook_interleaved_parked_continuations_on_core() {
    notebook_interleaved_parked_continuations(EngineKind::Core);
}

#[test]
fn notebook_interleaved_parked_continuations_on_prepared_stg() {
    notebook_interleaved_parked_continuations(EngineKind::Prepared);
}

/// Byte-backed host answers -- `Text` over a fresh byte array, and `Integer`
/// as `IS` and as multi-limb `IP` -- resuming parked turns on both engines.
/// On the prepared route the validator refuses invalid UTF-8 and a
/// non-canonical `BigNat#` (limbs whose value fits `IS`) before the frame is
/// touched.
fn notebook_byte_answers(engine: EngineKind) {
    use tidepool_bridge::{ToCore, Value};
    use tidepool_repr::Literal;

    let mut notebook = Notebook::new(engine);
    // Brings `Text` into the session table.
    let rendered = notebook
        .expression("T.length (\"abc\" :: Text)")
        .to_string();
    assert!(
        rendered.contains('3'),
        "{engine:?}: warm-up rendered {rendered}"
    );
    let table = notebook.last_table.clone().expect("an expression turn ran");
    let text_id = notebook.constructor("Text");
    // Brings `IS`/`IP` into the session table.
    let rendered = notebook
        .expression("(2 :: Integer) ^ (70 :: Int) > 0")
        .to_string();
    assert!(
        rendered.contains("true"),
        "{engine:?}: warm-up rendered {rendered}"
    );
    let is_id = notebook.constructor("IS");
    let ip_id = notebook.constructor("IP");
    let in_id = notebook.constructor("IN");

    // --- t <- runLLMTurn @Text ---
    let (t_binder, t_hole) = notebook.suspend_ask(
        engine,
        "t",
        "(runLLMTurn @Text \"say something\" :: M Text)",
    );
    assert_eq!(notebook.session.parked_count(), 1);
    if engine == EngineKind::Prepared {
        let invalid = Value::Con(
            text_id,
            vec![
                Value::Lit(Literal::LitByteArray(vec![0xff, 0xfe])),
                Value::Lit(Literal::LitInt(0)),
                Value::Lit(Literal::LitInt(2)),
            ],
        );
        notebook.assert_refusal_leaves_frame_parked(
            &t_hole,
            invalid,
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerShape { .. })
                )
            },
            1,
        );
    }
    let answer = "héllo, wörld"
        .to_string()
        .to_value(&table)
        .expect("Text answer");
    notebook.resume_bind(t_hole, &t_binder, answer);
    assert_eq!(notebook.session.parked_count(), 0);
    let rendered = notebook.expression("T.length t").to_string();
    assert!(
        rendered.contains("12"),
        "{engine:?}: T.length t rendered {rendered}"
    );
    let rendered = notebook.expression("T.toUpper t").to_string();
    assert!(
        rendered.contains("HÉLLO, WÖRLD"),
        "{engine:?}: toUpper rendered {rendered}"
    );

    // --- n <- runLLMTurn @Integer, answered with a multi-limb IP ---
    let (n_binder, n_hole) = notebook.suspend_ask(
        engine,
        "n",
        "(runLLMTurn @Integer \"how many\" :: M Integer)",
    );
    if engine == EngineKind::Prepared {
        // One limb that fits `IS` is not a canonical `IP`.
        let refused = notebook
            .session
            .resume(
                n_hole.clone(),
                Value::Con(
                    ip_id,
                    vec![Value::Lit(Literal::LitByteArray(
                        7_u64.to_le_bytes().to_vec(),
                    ))],
                ),
            )
            .expect_err("a small IP is not canonical");
        assert!(
            matches!(
                refused,
                ResidentError::Prepared(PreparedRuntimeError::AnswerShape { .. })
            ),
            "unexpected refusal: {refused}"
        );
        assert_eq!(notebook.session.parked_count(), 1);
    }
    // 2^70 = limbs [0, 64] little-endian.
    let limbs: Vec<u8> = [0_u64, 1 << 6]
        .iter()
        .flat_map(|l| l.to_le_bytes())
        .collect();
    notebook.resume_bind(
        n_hole,
        &n_binder,
        Value::Con(ip_id, vec![Value::Lit(Literal::LitByteArray(limbs))]),
    );
    let rendered = notebook.expression("n `div` (2 ^ (60 :: Int))").to_string();
    assert!(
        rendered.contains("1024"),
        "{engine:?}: n div rendered {rendered}"
    );

    // --- m <- runLLMTurn @Integer, answered with IS and with IN ---
    let (m_binder, m_hole) = notebook.suspend_ask(
        engine,
        "m",
        "(runLLMTurn @Integer \"how few\" :: M Integer)",
    );
    notebook.resume_bind(
        m_hole,
        &m_binder,
        Value::Con(is_id, vec![Value::Lit(Literal::LitInt(-5))]),
    );
    let rendered = notebook
        .expression("m * n `div` (2 ^ (60 :: Int))")
        .to_string();
    assert!(
        rendered.contains("-5120"),
        "{engine:?}: m*n rendered {rendered}"
    );
    let _ = in_id;
    assert_eq!(notebook.session.parked_count(), 0);
}

#[test]
fn notebook_byte_answers_on_core() {
    notebook_byte_answers(EngineKind::Core);
}

#[test]
fn notebook_byte_answers_on_prepared_stg() {
    notebook_byte_answers(EngineKind::Prepared);
}

/// The step-2 exit scenario end to end: a decl-plane function, a value-plane
/// partial application of it, a typed ask parked while sibling work runs
/// against both, a host-built resume, and a final turn that composes the
/// declared function, the partial application, and the resumed binder
/// together -- checking the value plane still resolves every binder
/// afterward. Both engines run the exact same turns through the exact same
/// `Notebook`.
fn notebook_end_to_end(engine: EngineKind) {
    use tidepool_bridge::Value;

    let mut notebook = Notebook::new(engine);
    assert_eq!(notebook.session.engine_kind(), engine);

    // 1. A decl-plane function, visible to every later turn.
    notebook.declare("addN :: Int -> Int -> Int\naddN n x = x + n\n");

    // 2. A partial application of it, bound on the value plane; a later turn
    // applies it.
    let inc_binder = notebook.bind("inc <- pure (addN 1)");
    assert_eq!(inc_binder.name, "inc");
    let rendered = notebook.expression("inc 41").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: inc 41 rendered as {rendered}"
    );

    // Brings `True`/`False` into the session table for the host-built answer
    // below, exactly as `notebook_suspension` and `notebook_data_answers` do.
    let rendered = notebook.expression("not False").to_string();
    assert!(
        rendered.contains("true"),
        "{engine:?}: not False rendered as {rendered}"
    );
    let true_id = notebook.constructor("True");

    // 3. Park on a typed ask.
    let (b_binder, hole) = notebook.suspend_ask(engine, "b", "(runLLMTurn @Bool \"q\" :: M Bool)");
    assert_eq!(notebook.session.parked_count(), 1);

    // 4. Sibling work while parked -- against both the partial application
    // and the declared function -- leaves the frame parked.
    let rendered = notebook.expression("inc 1").to_string();
    assert!(
        rendered.contains('2'),
        "{engine:?}: inc 1 rendered as {rendered}"
    );
    assert_eq!(notebook.session.parked_count(), 1);

    // 5. Resume with a host-built answer.
    notebook.resume_bind(hole, &b_binder, Value::Con(true_id, Vec::new()));
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");

    // 6. A later turn uses everything: the resumed binder, the partial
    // application, and the declared function called directly.
    let rendered = notebook.expression("if b then inc 9 else 0").to_string();
    assert!(
        rendered.contains("10"),
        "{engine:?}: if b then inc 9 else 0 rendered as {rendered}"
    );
    let rendered = notebook.expression("addN 2 40").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: addN 2 40 rendered as {rendered}"
    );
    assert!(
        notebook
            .session
            .current_binding_in(tidepool_codegen::scope::ScopeId::ROOT, "inc")
            .is_some(),
        "{engine:?}: inc is not bound"
    );
    assert!(
        notebook
            .session
            .current_binding_in(tidepool_codegen::scope::ScopeId::ROOT, "b")
            .is_some(),
        "{engine:?}: b is not bound"
    );

    // 7. Engine-neutral counts, left where the turn found them.
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");
}

#[test]
fn notebook_end_to_end_on_core() {
    notebook_end_to_end(EngineKind::Core);
}

#[test]
fn notebook_end_to_end_on_prepared_stg() {
    notebook_end_to_end(EngineKind::Prepared);
}

impl Notebook {
    /// Run `text` (an expression or single-binder bind turn) to the
    /// suspension of an ordinary effect request: one that carries no
    /// `typedSite`, so the prepared route must classify it by its request
    /// constructor. Returns the hole, the rendered request, and the turn's
    /// binder when `text` is a bind.
    fn suspend_ordinary(
        &mut self,
        engine: EngineKind,
        text: &str,
    ) -> (
        tidepool_runtime::session::ResidentHole,
        serde_json::Value,
        Option<BoundBinder>,
    ) {
        let (outcome, binder, table) = match self.compile(text) {
            TurnResult::Expr { compiled, .. } => {
                let outcome = self
                    .session
                    .run_with_sites("notebook_ordinary", compiled.code())
                    .unwrap_or_else(|error| panic!("{engine:?}: {text:?} failed to run: {error}"));
                (outcome, None, compiled.code().table.clone())
            }
            TurnResult::Bind {
                bound, compiled, ..
            } => {
                let [binder] = bound.as_slice() else {
                    panic!("{engine:?}: {text:?} bound {} names", bound.len());
                };
                let outcome = self
                    .session
                    .run_bind_with_sites(
                        "notebook_ordinary",
                        compiled.code(),
                        binder,
                        Generation(self.generation),
                    )
                    .unwrap_or_else(|error| panic!("{engine:?}: {text:?} failed to run: {error}"));
                (outcome, Some(binder.clone()), compiled.code().table.clone())
            }
            _ => panic!("{engine:?}: {text:?} is neither an expression nor a bind"),
        };
        let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
            panic!("{engine:?}: {text:?} did not suspend: {outcome:?}");
        };
        let request = tidepool_runtime::value_to_json(&request, &table, 0);
        assert_eq!(
            typed_site_of(&request),
            None,
            "{engine:?}: the ordinary request {request} carries a typedSite"
        );
        self.last_table = Some(table);
        (hole, request, binder)
    }
}

/// Ordinary effect requests -- effect-GADT constructors with no dynamic
/// site -- park and resume on both engines with identical results. On the
/// prepared route the request is classified by its constructor through the
/// machine's verb index and answered at the constructor's synthetic reply
/// site: `say` takes a host-built `()` and `readFile` takes either side of
/// `Either FsError Text`. A `Value`-carrying reply (`kvGet`) still parks
/// and is refused as unconstructible with the frame intact.
fn notebook_ordinary_effects(engine: EngineKind) {
    use tidepool_bridge::{ToCore, Value};
    use tidepool_repr::Literal;

    let mut notebook = Notebook::new(engine);
    let rendered = notebook.expression("41 + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: warm-up rendered {rendered}"
    );
    let handles_before = notebook.session.value_handle_count();
    assert_eq!(notebook.session.parked_count(), 0);

    // --- Slice A: `say` parks on `Print` and resumes with `()`. ---
    let (hole, request, _) = notebook.suspend_ordinary(engine, "say \"hi\" >> pure (42 :: Int)");
    assert!(
        request.to_string().contains("hi"),
        "{engine:?}: the Print request rendered as {request}"
    );
    assert_eq!(notebook.session.parked_count(), 1, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 1, "{engine:?}");
    // Built exactly as the actor workbench's `resume_unit` builds it.
    let unit = ().to_value(notebook.session.data_con_table()).expect("() is in the session table");
    let outcome = notebook
        .session
        .resume(hole, unit)
        .unwrap_or_else(|error| panic!("{engine:?}: resuming say failed: {error}"));
    let ResidentOutcome::Completed { result, .. } = outcome else {
        panic!("{engine:?}: the resumed say turn did not complete: {outcome:?}");
    };
    let rendered = tidepool_runtime::value_to_json(result.value(), result.table(), 0).to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: the say turn completed with {rendered}"
    );
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");
    assert_eq!(
        notebook.session.value_handle_count(),
        handles_before,
        "{engine:?}: the resumed say turn leaked a value handle"
    );

    // --- Slice B: `readFile` answered with `Right` and with `Left`. ---
    let (hole, request, binder) = notebook.suspend_ordinary(engine, "c <- readFile \"notes.txt\"");
    assert!(
        request.to_string().contains("notes.txt"),
        "{engine:?}: the FsRead request rendered as {request}"
    );
    let binder = binder.expect("a bind turn");
    let table = notebook.last_table.clone().expect("the ask turn's table");
    let text = |value: &str| value.to_string().to_value(&table).expect("Text answer");
    let right_id = notebook.constructor("Right");
    let left_id = notebook.constructor("Left");
    let not_found_id = notebook.constructor("FsNotFound");
    notebook.resume_bind(hole, &binder, Value::Con(right_id, vec![text("contents")]));
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    let rendered = notebook
        .expression("either (const \"failed\") id c")
        .to_string();
    assert!(
        rendered.contains("contents"),
        "{engine:?}: the Right answer rendered as {rendered}"
    );

    let (hole, _, binder) = notebook.suspend_ordinary(engine, "d <- readFile \"missing.txt\"");
    let binder = binder.expect("a bind turn");
    notebook.resume_bind(
        hole,
        &binder,
        Value::Con(
            left_id,
            vec![Value::Con(not_found_id, vec![text("missing.txt")])],
        ),
    );
    let rendered = notebook
        .expression("case d of { Left (FsNotFound p) -> p; _ -> \"other\" }")
        .to_string();
    assert!(
        rendered.contains("missing.txt"),
        "{engine:?}: the Left answer rendered as {rendered}"
    );
    assert_eq!(notebook.session.parked_count(), 0, "{engine:?}");
    assert_eq!(notebook.session.stowed_roots_count(), 0, "{engine:?}");

    // --- Deferred: a `Value`-carrying reply parks and is refused. ---
    // `kvGet`'s reply is `Maybe Value`. The `Maybe` and `Value` layers are
    // ordinary data evidence, so `Nothing` is answerable; an `Object` must
    // build aeson's `KeyMap` spine, whose evidence is unconstructible, and is
    // refused with the frame intact.
    if engine == EngineKind::Prepared {
        let (hole, request, binder) = notebook.suspend_ordinary(engine, "e <- kvGet \"key\"");
        assert!(
            request.to_string().contains("key"),
            "{engine:?}: the KvGet request rendered as {request}"
        );
        let binder = binder.expect("a bind turn");
        let nothing_id = notebook.constructor("Nothing");
        let just_id = notebook.constructor("Just");
        let object_id = notebook.constructor("Object");
        notebook.assert_refusal_leaves_frame_parked(
            &hole,
            Value::Con(
                just_id,
                vec![Value::Con(object_id, vec![Value::Lit(Literal::LitInt(0))])],
            ),
            |error| {
                matches!(
                    error,
                    ResidentError::Prepared(PreparedRuntimeError::AnswerUnconstructible { .. })
                )
            },
            1,
        );
        notebook.resume_bind(hole, &binder, Value::Con(nothing_id, Vec::new()));
        assert_eq!(notebook.session.parked_count(), 0);
        assert_eq!(notebook.session.stowed_roots_count(), 0);
        let rendered = notebook.expression("isNothing e").to_string();
        assert!(
            rendered.contains("true"),
            "{engine:?}: isNothing e rendered as {rendered}"
        );
    }

    // The session stays usable.
    let rendered = notebook.expression("40 + 2").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: 40 + 2 rendered as {rendered}"
    );
}

#[test]
fn notebook_ordinary_effects_on_core() {
    notebook_ordinary_effects(EngineKind::Core);
}

#[test]
fn notebook_ordinary_effects_on_prepared_stg() {
    notebook_ordinary_effects(EngineKind::Prepared);
}
