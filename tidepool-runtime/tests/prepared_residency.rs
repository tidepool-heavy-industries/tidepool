//! S4a acceptance: a prepared session's between-turn quiescent point
//! actually drains a major collection, so residency stays bounded across
//! many turns instead of growing one program per turn forever.
//!
//! Needs a resolvable `$TIDEPOOL_EXTRACT` and its Haskell worker (as
//! `prepared_turn.rs` does); no Core-route counterpart -- `residency()` is
//! `None` on Core, so there is nothing to assert there.

use std::path::{Path, PathBuf};

use tidepool_repr::Generation;
use tidepool_runtime::session::{
    resident_workbench_templates, run_turn, BoundBinder, EngineKind, ModuleEnv, ResidentOutcome,
    ResidentSession, SessionLib, TurnRequest, TurnResult, TurnTemplate,
};
use tidepool_testing::eval_harness;

/// The minimal parts of `prepared_turn.rs`'s own `Notebook` this file needs:
/// one resident session, its compile plumbing, and expression/bind turns.
/// Copied rather than shared so this file never has to touch
/// `prepared_turn.rs`.
struct Notebook {
    session: ResidentSession<frunk::HNil, tidepool_mcp::CapturedOutput>,
    preamble: String,
    effect_stack: String,
    include: Vec<PathBuf>,
    root: tempfile::TempDir,
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
        let root = tempfile::tempdir().expect("session root");
        let lib = SessionLib::open(
            tidepool_repr::SessionId(1),
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

    /// Run an expression turn to completion; the value itself is not needed.
    fn expression(&mut self, text: &str) {
        let TurnResult::Expr { compiled, .. } = self.compile(text) else {
            panic!("{text:?} did not classify as an expression");
        };
        let outcome = self
            .session
            .run_with_sites("residency_expression", compiled.code())
            .unwrap_or_else(|error| panic!("{text:?} failed to run: {error}"));
        assert!(
            matches!(outcome, ResidentOutcome::Completed { .. }),
            "{text:?} did not complete: {outcome:?}"
        );
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
        let outcome = self
            .session
            .run_bind_with_sites(
                "residency_bind",
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

/// Forty `1 + <i>` expression turns on the prepared route: after a warm-up
/// of five turns (the first few installs may still be settling machine
/// bootstrap/site-index bookkeeping, and descriptor-arena compaction commits
/// before retirement per the lifetime contract's decision 7, so a retiring
/// program's bytes leave one collection later than its other counters),
/// every `ResidencyCounts` field -- `old_bytes` included, now that
/// descriptor-arena compaction has landed -- is flat turn over turn, and
/// `programs` never exceeds the number of live bindings plus one (the turn
/// currently running). A binding is then introduced (`x <- pure 1`) and
/// twenty more expression turns run importing nothing new: `programs` stays
/// flat there too -- the bound bind's own program does not accumulate one
/// retained program per subsequent turn.
#[test]
fn prepared_session_residency_stays_bounded_across_many_turns() {
    let mut notebook = Notebook::new(EngineKind::Prepared);

    const WARMUP: usize = 5;
    const TOTAL: usize = 40;
    let mut last_counts: Option<tidepool_codegen::prepared_program::ResidencyCounts> = None;
    let mut last_old_bytes: Option<usize> = None;
    for i in 0..TOTAL {
        notebook.expression(&format!("1 + {i}"));
        let counts = notebook
            .session
            .residency()
            .expect("the prepared route reports residency counts");
        assert!(
            counts.programs <= 1,
            "turn {i}: programs={} exceeds the bound (0 live bindings + 1)",
            counts.programs
        );
        if i + 1 > WARMUP {
            if let Some(previous) = last_counts {
                assert_eq!(
                    counts.block_words, previous.block_words,
                    "turn {i}: block_words grew past warm-up"
                );
                assert_eq!(
                    counts.persistent_roots, previous.persistent_roots,
                    "turn {i}: persistent_roots grew past warm-up"
                );
                assert_eq!(
                    counts.handles, previous.handles,
                    "turn {i}: handles grew past warm-up"
                );
                assert_eq!(
                    counts.parked, previous.parked,
                    "turn {i}: parked grew past warm-up"
                );
                assert_eq!(
                    counts.stack_map_links, previous.stack_map_links,
                    "turn {i}: stack_map_links grew past warm-up"
                );
                assert_eq!(
                    counts.static_regions, previous.static_regions,
                    "turn {i}: static_regions grew past warm-up"
                );
                assert_eq!(
                    counts.descriptor_rows, previous.descriptor_rows,
                    "turn {i}: descriptor_rows grew past warm-up"
                );
                assert_eq!(
                    counts.callable_rows, previous.callable_rows,
                    "turn {i}: callable_rows grew past warm-up"
                );
                assert_eq!(
                    counts.enter_rows, previous.enter_rows,
                    "turn {i}: enter_rows grew past warm-up"
                );
            }
            let old_bytes = notebook
                .session
                .old_bytes()
                .expect("the prepared route reports old-space bytes");
            if let Some(previous) = last_old_bytes {
                assert_eq!(old_bytes, previous, "turn {i}: old_bytes grew past warm-up");
            }
            last_old_bytes = Some(old_bytes);
        }
        last_counts = Some(counts);
    }

    // Introduce a live binding, then keep running: the bound value's
    // producing program may or may not stay resident (it depends on whether
    // the binding's value keeps any of its code reachable), but no further
    // program should accumulate turn over turn.
    notebook.bind("x <- pure 1");
    let after_bind = notebook
        .session
        .residency()
        .expect("the prepared route reports residency counts");

    let mut last_programs = after_bind.programs;
    for i in 0..20 {
        notebook.expression(&format!("x + {i}"));
        let counts = notebook
            .session
            .residency()
            .expect("the prepared route reports residency counts");
        assert!(
            counts.programs <= 2,
            "post-bind turn {i}: programs={} exceeds the bound (1 live binding + 1)",
            counts.programs
        );
        assert_eq!(
            counts.programs, last_programs,
            "post-bind turn {i}: programs grew turn over turn"
        );
        last_programs = counts.programs;
    }
}
