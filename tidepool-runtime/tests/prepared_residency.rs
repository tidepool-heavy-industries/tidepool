//! S4a acceptance: a prepared session's between-turn quiescent point
//! actually drains a major collection, so residency stays bounded across
//! many turns instead of growing one program per turn forever.
//!
//! The collection itself is amortized (`PreparedEngine::quiesce_and_collect`,
//! `tidepool-runtime/src/session/prepared.rs`): it only actually runs a
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

/// Fixture mirror of `PreparedEngine::MAJOR_COLLECTION_INSTALL_INTERVAL`
/// (`tidepool-runtime/src/session/prepared.rs`) -- see the module doc for
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

/// Twenty `1 + <i>` expression turns on the prepared route, each installing
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
    let mut notebook = Notebook::new(EngineKind::Prepared);

    // A multiple of `N`, so the phase ends exactly on a collection boundary
    // and the bind below is the first install of the next window.
    const TOTAL: usize = 20;
    let live_bindings = 0;
    let mut boundary_counts: Option<tidepool_codegen::prepared_program::ResidencyCounts> = None;
    let mut boundary_old_bytes: Option<usize> = None;
    let mut boundaries_seen = 0;
    for i in 0..TOTAL {
        notebook.expression(&format!("1 + {i}"));
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
    for i in 0..POST_BIND {
        notebook.expression(&format!("x + {i}"));
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
/// old-space growth check (`tidepool-runtime/src/session/prepared.rs`)
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
    let mut notebook = Notebook::new(EngineKind::Prepared);

    for i in 0..N {
        notebook.expression(&format!("1 + {i}"));
    }
    let old_bytes_before = notebook
        .session
        .old_bytes()
        .expect("the prepared route reports old-space bytes");

    notebook.bind("xs <- (let ys = [1..200000 :: Int] in seq (length ys) (pure ys))");

    let mut collected_within = None;
    for i in 0..2 {
        notebook.expression(&format!("2 + {i}"));
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
