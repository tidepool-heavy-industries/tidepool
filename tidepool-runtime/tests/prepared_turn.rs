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
    Generation,
};
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, BoundBinder, EngineKind, PreparedRuntimeError,
    ResidentError, ResidentOutcome, ResidentSession, TurnRequest, TurnResult, TurnTemplate,
    ValueTier,
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
        let session = ResidentSession::unbootstrapped_on(
            engine,
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
            include.clone(),
            tidepool_runtime::DEFAULT_NURSERY_SIZE,
            None,
        );
        Self {
            session,
            preamble,
            effect_stack,
            include,
            root: tempfile::tempdir().expect("session root"),
            injected: Vec::new(),
            generation: 0,
            last_table: None,
        }
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

    /// Run the ask turn `b <- runLLMTurn @Bool "q"` to its suspension and
    /// return the binder and hole, checking that the request names one of the
    /// turn's declared sites and, on the prepared route, that the artifact
    /// admits the resume entry.
    fn suspend_ask(
        &mut self,
        engine: EngineKind,
    ) -> (BoundBinder, tidepool_runtime::session::ResidentHole) {
        let TurnResult::Bind {
            bound, compiled, ..
        } = self.compile("b <- (runLLMTurn @Bool \"q\" :: M Bool)")
        else {
            panic!("{engine:?}: the ask did not classify as a bind");
        };
        let [binder] = bound.as_slice() else {
            panic!("{engine:?}: the ask bound {} names", bound.len());
        };
        assert_eq!(binder.name, "b");
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
            "{engine:?}: the ask declares no site"
        );
        let outcome = self
            .session
            .run_bind_with_sites(
                "notebook_ask",
                compiled.code(),
                binder,
                Generation(self.generation),
            )
            .unwrap_or_else(|error| panic!("{engine:?}: the ask failed to run: {error}"));
        let ResidentOutcome::Suspended { hole, request, .. } = outcome else {
            panic!("{engine:?}: the ask did not suspend: {outcome:?}");
        };
        let request = tidepool_runtime::value_to_json(&request, code.table, 0);
        let site = typed_site_of(&request)
            .unwrap_or_else(|| panic!("{engine:?}: the request names no typedSite: {request}"));
        assert!(
            declared_sites.contains(&site),
            "{engine:?}: site {site} is not one of the turn's {declared_sites:?}"
        );
        assert_eq!(self.session.parked_holes(), vec![hole.cont_id()]);
        assert_eq!(self.session.parked_count(), 1);
        assert_eq!(self.session.stowed_roots_count(), 1);
        (binder.clone(), hole)
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

    let (binder, hole) = notebook.suspend_ask(engine);

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
        let handles_parked = notebook.session.value_handle_count();
        let roots_parked = notebook.session.persistent_roots_count();
        let literal = notebook
            .session
            .resume(hole.clone(), Value::Lit(Literal::LitInt(1)))
            .expect_err("a literal is not a Bool");
        assert!(
            matches!(
                literal,
                ResidentError::Prepared(PreparedRuntimeError::AnswerShape { .. })
            ),
            "unexpected refusal: {literal}"
        );
        let wrong_family = notebook
            .session
            .resume(
                hole.clone(),
                Value::Con(boxed_int, vec![Value::Lit(Literal::LitInt(1))]),
            )
            .expect_err("an Int constructor is not a Bool");
        assert!(
            matches!(
                wrong_family,
                ResidentError::Prepared(PreparedRuntimeError::AnswerConstructor { .. })
            ),
            "unexpected refusal: {wrong_family}"
        );
        assert_eq!(notebook.session.parked_holes(), vec![hole.cont_id()]);
        assert_eq!(notebook.session.parked_count(), 1);
        assert_eq!(notebook.session.stowed_roots_count(), 1);
        assert_eq!(notebook.session.value_handle_count(), handles_parked);
        assert_eq!(notebook.session.persistent_roots_count(), roots_parked);
    }

    // The host-built answer completes the bind through the frame's resume
    // entry; the bound value is the answer.
    let outcome = notebook
        .session
        .resume(hole.clone(), Value::Con(true_id, Vec::new()))
        .unwrap_or_else(|error| panic!("{engine:?}: resuming with True failed: {error}"));
    assert!(
        matches!(outcome, ResidentOutcome::Completed { .. }),
        "{engine:?}: the resumed bind did not complete: {outcome:?}"
    );
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
    notebook.injected.push(binder.module.clone());
    let rendered = notebook.expression("not b").to_string();
    assert!(
        rendered.contains("false"),
        "{engine:?}: not b rendered as {rendered}"
    );

    // A second ask, this time aborted.
    let handles_before = notebook.session.value_handle_count();
    let roots_before = notebook.session.persistent_roots_count();
    let (_, hole) = notebook.suspend_ask(engine);

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

/// Run a single-binder ask turn `<binder> <- <expr>` to its suspension,
/// checking (as `Notebook::suspend_ask` does for the `Bool` ask) that the
/// request names one of the turn's declared sites and, on the prepared
/// route, that the artifact admits the resume entry. Generalizes
/// `suspend_ask` over the binder name and the ask expression so the data-
/// and `Maybe`-shaped answer turns below can reuse the same idiom.
fn suspend_typed_ask(
    notebook: &mut Notebook,
    engine: EngineKind,
    binder_name: &str,
    expr: &str,
) -> (BoundBinder, tidepool_runtime::session::ResidentHole) {
    let TurnResult::Bind {
        bound, compiled, ..
    } = notebook.compile(&format!("{binder_name} <- {expr}"))
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
    let outcome = notebook
        .session
        .run_bind_with_sites(
            "notebook_typed_ask",
            compiled.code(),
            binder,
            Generation(notebook.generation),
        )
        .unwrap_or_else(|error| panic!("{engine:?}: the {binder_name} ask failed to run: {error}"));
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
    let (n_binder, n_hole) = suspend_typed_ask(
        &mut notebook,
        engine,
        "n",
        "(runLLMTurn @Int \"how many\" :: M Int)",
    );
    assert_eq!(notebook.session.parked_count(), 1);
    assert_eq!(notebook.session.stowed_roots_count(), 1);

    if engine == EngineKind::Prepared {
        let handles_parked = notebook.session.value_handle_count();
        let roots_parked = notebook.session.persistent_roots_count();
        let literal = notebook
            .session
            .resume(n_hole.clone(), Value::Lit(Literal::LitInt(7)))
            .expect_err("a bare literal is not the I# constructor");
        assert!(
            matches!(
                literal,
                ResidentError::Prepared(PreparedRuntimeError::AnswerShape { .. })
            ),
            "unexpected refusal: {literal}"
        );
        let wrong_family = notebook
            .session
            .resume(n_hole.clone(), Value::Con(true_id, Vec::new()))
            .expect_err("a Bool constructor is not an Int");
        assert!(
            matches!(
                wrong_family,
                ResidentError::Prepared(PreparedRuntimeError::AnswerConstructor { .. })
            ),
            "unexpected refusal: {wrong_family}"
        );
        assert_eq!(notebook.session.parked_holes(), vec![n_hole.cont_id()]);
        assert_eq!(notebook.session.parked_count(), 1);
        assert_eq!(notebook.session.stowed_roots_count(), 1);
        assert_eq!(notebook.session.value_handle_count(), handles_parked);
        assert_eq!(notebook.session.persistent_roots_count(), roots_parked);
    }

    let outcome = notebook
        .session
        .resume(
            n_hole.clone(),
            Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(41))]),
        )
        .unwrap_or_else(|error| panic!("{engine:?}: resuming n with 41 failed: {error}"));
    assert!(
        matches!(outcome, ResidentOutcome::Completed { .. }),
        "{engine:?}: the resumed n bind did not complete: {outcome:?}"
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

    notebook.injected.push(n_binder.module.clone());
    let rendered = notebook.expression("n + 1").to_string();
    assert!(
        rendered.contains("42"),
        "{engine:?}: n + 1 rendered as {rendered}"
    );

    // --- m <- runLLMTurn @(Maybe Int): a `Data` site whose rows are
    // `Just`/`Nothing`, `Just` carrying a nested `I#` field. ---
    let handles_before = notebook.session.value_handle_count();
    let (m_binder, m_hole) = suspend_typed_ask(
        &mut notebook,
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

    let outcome = notebook
        .session
        .resume(
            m_hole.clone(),
            Value::Con(
                just_id,
                vec![Value::Con(i_hash_id, vec![Value::Lit(Literal::LitInt(4))])],
            ),
        )
        .unwrap_or_else(|error| panic!("{engine:?}: resuming m with Just 4 failed: {error}"));
    assert!(
        matches!(outcome, ResidentOutcome::Completed { .. }),
        "{engine:?}: the resumed m bind did not complete: {outcome:?}"
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

    notebook.injected.push(m_binder.module.clone());
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
