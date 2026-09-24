//! What one notebook unit costs a resident prepared session after its
//! Haskell compile is already done.
//!
//! A resident session compiles one prepared program per unit and installs it
//! on the session's one long-lived machine. This suite runs a sequence of
//! units through the production owners (`run_turn` over the shared workbench
//! templates, then `ResidentSession`) and reports, per unit, the wall time of
//! each stage and the Cranelift work the install caused
//! (`ResidentSession::codegen_totals`).
//!
//! Needs a resolvable `$TIDEPOOL_EXTRACT` and its Haskell worker
//! (`just test-target tidepool-runtime session 'test(prepared_unit_codegen_cost)'`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tidepool_repr::{Generation, SessionId};
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, ModuleEnv, ResidentOutcome, ResidentSession,
    SessionLib, TurnRequest, TurnResult, TurnTemplate,
};
use tidepool_testing::eval_harness;

/// One unit's attribution: what GHC cost, what the install cost, what the run
/// cost, and how much Cranelift work the install caused.
#[derive(Debug, Clone)]
struct UnitCost {
    label: &'static str,
    compile: Duration,
    run: Duration,
    functions: u64,
    code_bytes: u64,
    /// Globals this unit's program declared instead of projecting a body.
    imports: usize,
    /// Package tops the machine can hand a later unit after this one.
    exports: usize,
}

impl UnitCost {
    fn report(&self, index: usize) {
        println!(
            "[unit-cost] {index} {:<28} compile={:>6}ms run={:>6}ms functions={:>5} \
             code_bytes={:>8} imports={:>5} exports={:>5}",
            self.label,
            self.compile.as_millis(),
            self.run.as_millis(),
            self.functions,
            self.code_bytes,
            self.imports,
            self.exports,
        );
    }
}

/// A minimal notebook over one resident prepared session, tracking the value
/// generation and bound value modules a later unit imports, exactly as the
/// actor workbench's compile view does.
struct Notebook {
    session: ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    preamble: String,
    effect_stack: String,
    include: Vec<PathBuf>,
    root: tempfile::TempDir,
    injected: Vec<String>,
    generation: u64,
    costs: Vec<UnitCost>,
}

/// Print the `prepared compile` and `prepared install` attribution events to
/// stdout, so a `--no-capture` run reads the per-stage breakdown beside the
/// per-unit table. Ignored if a subscriber is already installed.
fn install_stage_trace() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let filter = tracing_subscriber::EnvFilter::new(
        "warn,tidepool_codegen::prepared_compile=info,tidepool_runtime::prepared_install=info",
    );
    // best-effort: a prior test in the process may already own the global
    // subscriber; this one is then simply not installed.
    drop(
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_target(true),
            )
            .try_init(),
    );
}

/// Group a turn's prepared tops by defining unit and module, so the shape of
/// what each unit re-projects is visible: which modules the projection keeps
/// bodies for, and how many tops each contributes.
fn dump_tops(prepared: &tidepool_repr::execution_schema::PreparedProgram) {
    use std::collections::BTreeMap;
    use tidepool_repr::execution_schema::Group;
    let mut by_module: BTreeMap<(String, String), usize> = BTreeMap::new();
    for group in prepared.bindings() {
        let tops = match group {
            Group::NonRecursive(top) => std::slice::from_ref(top),
            Group::Recursive(tops) => tops.as_slice(),
        };
        for top in tops {
            *by_module
                .entry((top.identity.unit.clone(), top.identity.module.clone()))
                .or_default() += 1;
        }
    }
    let total: usize = by_module.values().sum();
    println!("[tops] {total} tops across {} modules", by_module.len());
    let mut rows: Vec<_> = by_module.into_iter().collect();
    rows.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    for ((unit, module), count) in rows {
        println!("[tops]   {count:>5}  unit={unit} module={module}");
    }
    println!("[tops] globals declared: {}", prepared.globals().len());
}

impl Notebook {
    fn new() -> Self {
        install_stage_trace();
        eval_harness::require_extract();
        let decls = tidepool_mcp::standard_decls();
        let preamble = tidepool_mcp::build_preamble(&decls, false);
        let effect_stack = tidepool_mcp::build_effect_stack_type(&decls);
        let mut include = eval_harness::effects_include().to_vec();
        include.push(eval_harness::prelude_path());
        let root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            SessionId(1),
            root.path().join("decl-lib"),
            ModuleEnv::standalone_default(),
        )
        .expect("open decl plane")
        .with_validation_include(vec![eval_harness::prelude_path()]);
        include.push(lib.include_dir().to_path_buf());
        let session = ResidentSession::unbootstrapped(
            frunk::HNil,
            tidepool_mcp::CapturedOutput::new(),
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
            costs: Vec::new(),
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
            session_id: None,
            turn_text: text,
            templates: &templates,
            include: &include,
            session_root: self.root.path(),
            inject_modules: &self.injected,
            gen: self.generation,
            verdict: None,
            target: None,
            retained_imports: &retained,
        })
        .unwrap_or_else(|failure| {
            panic!(
                "{text:?} failed to compile: {}",
                tidepool_runtime::classify_compile(&failure.error).message
            )
        })
    }

    /// Run one bind unit, recording its compile, run, and codegen cost.
    fn unit(&mut self, label: &'static str, text: &str) {
        let compile_started = Instant::now();
        let TurnResult::Bind {
            bound, compiled, ..
        } = self.compile(text)
        else {
            panic!("{text:?} did not classify as a bind");
        };
        let compile = compile_started.elapsed();
        if std::env::var_os("TIDEPOOL_UNIT_COST_DUMP_TOPS").is_some() {
            dump_tops(&compiled.prepared);
        }
        let [binder] = bound.as_slice() else {
            panic!("{text:?} bound {} names", bound.len());
        };
        let imports = compiled.prepared.globals().len();
        let before = self.session.codegen_totals().unwrap_or((0, 0));
        let run_started = Instant::now();
        let outcome = self
            .session
            .run_bind_with_sites(
                "unit_cost_bind",
                compiled.code(),
                binder,
                Generation(self.generation),
            )
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        let run = run_started.elapsed();
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "{text:?} did not complete: {outcome:?}"
        );
        let after = self
            .session
            .codegen_totals()
            .expect("a prepared session reports codegen totals once bootstrapped");
        self.injected.push(binder.module.clone());
        let cost = UnitCost {
            label,
            compile,
            run,
            functions: after.0 - before.0,
            code_bytes: after.1 - before.1,
            imports,
            exports: self
                .session
                .code_export_count()
                .expect("a prepared session reports its code exports once bootstrapped"),
        };
        cost.report(self.costs.len());
        self.costs.push(cost);
    }

    /// Run an expression turn and render its result as JSON: what the units
    /// actually computed, read back through the same imports.
    fn expression(&mut self, text: &str) -> serde_json::Value {
        let TurnResult::Expr { compiled, .. } = self.compile(text) else {
            panic!("{text:?} did not classify as an expression");
        };
        let outcome = self
            .session
            .run_with_sites("unit_cost_expression", compiled.code())
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        let ResidentOutcome::Completed { result, .. } = outcome else {
            panic!("{text:?} did not complete: {outcome:?}");
        };
        tidepool_runtime::value_to_json(result.value(), result.table(), 0)
    }
}

/// Five units in ONE resident session: two trivial binds of identical shape,
/// two that reach into the standard library, and one that only references
/// values bound by earlier units.
///
/// The assertion is about structural reuse, not an unstable fixed ratio: the
/// second trivial bind must import the earlier unit and generate less code
/// than the first unit that established the shared support.
#[test]
fn a_later_unit_reuses_an_earlier_unit_s_generated_code() {
    let mut notebook = Notebook::new();
    notebook.unit("trivial bind", "a <- pure (1 :: Int)");
    notebook.unit("trivial bind again", "b <- pure (2 :: Int)");
    notebook.unit(
        "library text",
        "c <- pure (T.length (T.toUpper (T.pack \"hello\")))",
    );
    notebook.unit("references earlier units", "d <- pure (a + b + c)");
    notebook.unit(
        "library list",
        "e <- pure (L.sum (L.sort [d, 3 :: Int, 1]))",
    );

    // Reuse is only a win if it still computes the right answer: every unit
    // above reached package code (`T.toUpper`, `L.sort`, `+`) through an
    // import of an earlier unit's compiled copy rather than its own.
    // a=1, b=2, c=length "HELLO"=5, d=8, e=sum (sort [8,3,1])=12.
    let rendered = notebook.expression("(a, b, c, d, e)").to_string();
    assert!(
        rendered.contains("[1,2,5,8,12]"),
        "the imported package code computed {rendered}"
    );

    let first = &notebook.costs[0];
    let second = &notebook.costs[1];
    assert!(
        first.functions > 0,
        "the first unit compiled no Cranelift functions at all: {first:?}"
    );
    assert!(
        second.imports > 0,
        "the second unit imported none of the first unit's published code: {second:?}"
    );
    assert!(
        second.functions < first.functions,
        "the second unit regenerated the first unit's code: \
         first={} functions, second={} functions",
        first.functions,
        second.functions,
    );
}
