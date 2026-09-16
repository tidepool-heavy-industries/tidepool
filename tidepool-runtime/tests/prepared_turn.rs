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

use tidepool_repr::Generation;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, BoundBinder, EngineKind, ResidentOutcome,
    ResidentSession, TurnRequest, TurnResult, TurnTemplate, ValueTier,
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
        tidepool_runtime::value_to_json(result.value(), result.table(), 0)
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
}

#[test]
fn notebook_turns_run_on_core() {
    notebook_turns(EngineKind::Core);
}

#[test]
fn notebook_turns_run_on_prepared_stg() {
    notebook_turns(EngineKind::Prepared);
}
