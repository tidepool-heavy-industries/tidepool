//! The authored dogfood harnesses in `harness-dogfooding/` typecheck against
//! today's generated effect surface.
//!
//! THE HAZARD. These are Haskell sources the driver loads at RUNTIME, not
//! cargo targets — nothing else in the workspace compiles them, so `cargo
//! check`, clippy, and the whole Rust suite are all blind to them. An authored
//! harness can therefore drift arbitrarily far from the effect surface it
//! calls (a verb that no longer exists, a record field renamed, a LOCKED
//! signature quietly changed, a sum arm the generic-JSON derive rejects) and
//! stay green indefinitely. `examples/harness/` is covered by ~8 driver tests;
//! without this file the `harness-dogfooding/` harnesses are covered by none.
//!
//! COST. One extract compile per harness ROW: the companion's narrow row is
//! its own compile, and dev-tree and recursive-companion share the driver's
//! full outer row (`outer_row_decls`) so the second of those is a memo hit
//! only insofar as its SOURCE differs — a row is what a compile is
//! parameterized by, and there is no bundling that avoids a distinct harness's
//! own compile. All live in the GHC-heavy tier (`.config/nextest.toml`'s
//! default-filter excludes `package(tidepool-harness) & kind(test)`
//! wholesale).
//!
//! SCOPE. Typecheck only. These probes force GHC through `render` and `loop`
//! — plus `resumeLoop` and the pure resume decisions for a harness that
//! declares them; they do not drive a model, spawn an agent, or touch a
//! repository.

mod support;

use std::path::PathBuf;
use tidepool_harness::engine::compile_turn;
use tidepool_harness::engine::EngineConfig;
use tidepool_runtime::compile_and_run_pure;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tidepool-harness has a parent (the repo root)")
        .to_path_buf()
}

/// Compile a probe module that imports the harness at `harness_dir` against
/// `decls`, forcing GHC through both locked entry points (`render` and
/// `loop`). Importing the module is already enough to typecheck every
/// declaration in it — GHC compiles a module whole — but naming both keeps
/// the probe honest about which contract is under test.
///
/// `extra_imports` and `extra_decls` are spliced into the probe's import block
/// and body respectively, for a harness that declares MORE than the universal
/// contract (dev-tree's `resumeLoop` and its pure resume decisions).
fn typecheck(
    harness_dir: &str,
    decls: Vec<tidepool_mcp::EffectDecl>,
    extra_imports: &str,
    extra_decls: &str,
) {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    let cfg = EngineConfig::from_decls(
        decls,
        repo_root().join("haskell/lib"),
        Some(repo_root().join(harness_dir)),
    )
    .expect("engine config for the dogfood row");
    let source = format!(
        concat!(
            "{{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, ",
            "FlexibleContexts, GADTs, ScopedTypeVariables, TypeApplications, LambdaCase, ",
            "RecordWildCards, OverloadedRecordDot, QuasiQuotes, DeriveGeneric, DeriveAnyClass #-}}\n",
            "module Probe where\n",
            "import Tidepool.Prelude hiding (render)\n",
            "import Tidepool.Effects\n",
            "import Harness\n",
            "{extra_imports}",
            "__probe :: M Text\n",
            "__probe = do {{ st <- loop initialState; pure (render st) }}\n",
            "{extra_decls}",
        ),
        extra_imports = extra_imports,
        extra_decls = extra_decls,
    );
    if let Err(e) = compile_turn(&cfg.extract_bin, &source, "__probe", &cfg.include, 0, 0) {
        // Debug form: the Display for a compile failure summarizes to a
        // diagnostic COUNT; the Debug form carries every diagnostic's text,
        // and a failing pin is exactly when someone needs them.
        panic!("{harness_dir} does not typecheck against today's surface:\n{e:#?}");
    }
}

/// The companion's row is the driver's own outer session: `[RunLLMTurn,
/// AskUser, Worktree, Subagent]` (mirrors `selfharness::driver::outer_decls`
/// — interposed effects FIRST so the suspend threshold stays 0; Worktree is
/// Subagent's hard companion). Its `loop` calls only `runLLMTurn` today —
/// conversation happens through the answerer's own `askUser` forms — but the
/// probe compiles against the full outer row the driver actually serves.
#[test]
fn companion_typechecks() {
    typecheck(
        "harness-dogfooding/companion",
        vec![
            tidepool_mcp::runllmturn_decl(),
            tidepool_mcp::askuser_decl(),
            tidepool_mcp::worktree_decl(),
            tidepool_mcp::subagent_decl(),
        ],
        "",
        "",
    );
}

/// The driver's full outer row (mirrors `selfharness::driver::outer_decls`),
/// shared by every probe that compiles against it — dev-tree's typecheck and
/// execution tests, and recursive-companion's typecheck — so no two of them
/// can compile a harness against different rows.
fn outer_row_decls() -> Vec<tidepool_mcp::EffectDecl> {
    vec![
        tidepool_mcp::runllmturn_decl(),
        tidepool_mcp::askuser_decl(),
        tidepool_mcp::console_decl(),
        tidepool_mcp::worktree_decl(),
        tidepool_mcp::event_decl(),
        tidepool_mcp::exec_decl(),
        tidepool_mcp::subagent_decl(),
        tidepool_mcp::journal_decl(),
        tidepool_mcp::green_decl(),
    ]
}

/// dev-tree is the executable design target of S1-L1: its row now IS the
/// driver's widened outer session (`[RunLLMTurn, AskUser, Console, Worktree,
/// RepoEvent, Exec, Subagent, Journal, Green]` — mirrors
/// `selfharness::driver::outer_decls`, interposed effects FIRST so the
/// suspend threshold stays 0). Typechecking against that exact row is what
/// turns this from "the file names only landed API" into "this compiles
/// against what the driver actually serves".
///
/// dev-tree also declares the OPT-IN second entry point
/// (`resumeLoop :: ResumeFold -> State -> Harness State`), so the probe names
/// both entries: a fresh boot compiles `loop`, a boot with a folded journal
/// compiles `resumeLoop`, and the driver picks between them. Naming
/// `resumeLoop` at its exact declared signature is what makes a drift in
/// either half a compile failure here rather than a boot refusal in
/// production.
///
/// The two pure resume decisions are named at their signatures for the same
/// reason the coalgebra's pure policy slots are pure: they decide from the
/// fold alone, with no agent and no git anywhere in the path, so they are
/// callable directly.
///
/// against what the driver actually serves". dev-tree itself calls no Green
/// verb — `Green` in the row with no authored call site is still a real
/// receipt: it proves `green_decl()` (and its `Tidepool.Async` auto-import)
/// compile clean alongside every other outer-row effect.
#[test]
fn dev_tree_typechecks() {
    typecheck(
        "harness-dogfooding/dev-tree",
        outer_row_decls(),
        "import Tidepool.Resume (ResumeFold, emptyResume)\nimport Chore (chorePlan, choreBudget)\nimport HarnessTypes (Budget (..), PlanApproval (..))\nimport Resume (descendantAmendPending, newestEntry, rescuePending)\nimport DevTreeJournal (JournalEvent)\nimport qualified Data.Text as T\n",
        concat!(
            "__resumeProbe :: M Text\n",
            "__resumeProbe = do { st <- resumeLoop emptyResume initialState; pure (render st) }\n",
            // The (Outcome -> Outcome) parameter is the fold ladder: the
            // journal's outcome wire stores PRE-ladder receipts, so every
            // doneness judgment in the resume decisions takes the ladder
            // explicitly (run 24: a raw-doneness check read a failed root's
            // bare receipt as Done and never rescued).
            "__resumeDecision :: (Outcome -> Outcome) -> ResumeFold -> Text -> DevPlan -> ResumePlan\n",
            "__resumeDecision = resumePlanFor\n",
            // The replan-rescue rule (run 24b): a journaled failed interior
            // outcome must not shadow a descendant branch's pending amendment.
            "__amendRescue :: (Outcome -> Outcome) -> ResumeFold -> DevPlan -> Bool\n",
            "__amendRescue = descendantAmendPending\n",
            "__newestEntry :: Maybe (Int, JournalEvent) -> Maybe (Int, JournalEvent) -> Maybe (Int, JournalEvent) -> Maybe (Int, JournalEvent)\n",
            "__newestEntry = newestEntry\n",
            "__proposeDecision :: Budget -> DevPlan -> Maybe Text\n",
            "__proposeDecision = proposalViolation\n",
            // Sprint mode: disjoint-item validation is pure and pinned.
            "__sprintDisjoint :: [DevPlan] -> Maybe Text\n",
            "__sprintDisjoint = sprintOverlap\n",
            "__planApprovalProbe :: PlanApproval\n",
            "__planApprovalProbe = PlanApproval { planApproved = True, revisionNote = \"approved\" }\n",
            // Deep-force the chore VALUES: a missing record field in Chore.hs
            // is a runtime bottom laziness would otherwise defer to mid-run
            // (live crash 2026-08-25) — Show forces every field at pin time.
            "__choreForce :: Int\n",
            "__choreForce = T.length (show chorePlan) + T.length (show choreBudget)\n",
        ),
    );
}

/// recursive-companion is the third dogfood harness, and it
/// compiles against the SAME full outer row dev-tree does — it declares no new
/// effect and asks for no row widening, using only `RunLLMTurn`, `AskUser`,
/// `Console` and `Journal` out of it.
///
/// The tree emerges from a session's own forks, serviced and budgeted by the
/// driver — there is no tree-building machinery here to pin. What is left to
/// pin is the locked entry-point contract itself, at exact signatures: the probe body
/// already drives `loop initialState`/`render`, and the `extra_decls` pin
/// `resumeLoop` (the opt-in resume entry the driver's structural scan
/// selects) and `rootPrompt` (the one per-turn request, exported for
/// scripted-provider keying).
#[test]
fn recursive_companion_typechecks() {
    typecheck(
        "harness-dogfooding/recursive-companion",
        outer_row_decls(),
        "import Tidepool.Resume (ResumeFold)\n",
        concat!(
            "__resumeLoop :: ResumeFold -> State -> Companion State\n",
            "__resumeLoop = resumeLoop\n",
            "__rootPrompt :: State -> Text\n",
            "__rootPrompt = rootPrompt\n",
            "__initialState :: State\n",
            "__initialState = initialState\n",
        ),
    );
}

/// Compile a PURE (no-`Eff`) target against `harness_dir` and run it on the
/// real JIT, returning its result as JSON (`tidepool_runtime::render::EvalResult::to_json`).
///
/// `decls` still has to name the harness's full assumed effect row — GHC
/// compiles the imported module WHOLE, so even a target that only touches
/// pure decision functions needs `Tidepool.Effects` generated at the row the
/// harness file itself was authored against, or the module fails to
/// typecheck before the pure part is ever reached.
fn execute_pure(
    harness_dir: &str,
    decls: Vec<tidepool_mcp::EffectDecl>,
    source: &str,
    target: &str,
) -> serde_json::Value {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    let cfg = EngineConfig::from_decls(
        decls,
        repo_root().join("haskell/lib"),
        Some(repo_root().join(harness_dir)),
    )
    .expect("engine config for the dogfood row");
    let include: Vec<_> = cfg.include.iter().map(|p| p.as_path()).collect();
    match compile_and_run_pure(source, target, &include) {
        Ok(result) => result.to_json(),
        Err(e) => panic!("{harness_dir}'s resume decisions did not run cleanly:\n{e}"),
    }
}

/// One `module ResumeDecisionProbe where` source: hand-built `ResumeFold`
/// values covering the decision table `resumePlanFor`/`newestEntry`
/// actually implement (read from `Harness.hs` directly, not assumed), folded
/// into one newline-separated `PASS`/`FAIL` report per case.
const RESUME_DECISION_SOURCE: &str = include_str!("probes/ResumeDecisionProbe.hs");

/// dev-tree's resume decisions, EXECUTED — not merely compiled.
///
/// `dev_tree_typechecks` above proves this module still compiles against
/// today's effect surface, including `resumeLoop` and the pure resume
/// decisions at their exact declared signatures — but it never calls them, so
/// a wrong PRECEDENCE (e.g. an older replan winning, a stale outcome beating
/// a newer split, or a recorded split
/// being silently re-derived instead of replayed) would compile clean and
/// only surface as corrupted state in a real crash-resume run. This is the
/// next rung: it builds `ResumeFold` values by hand — a recorded outcome, a
/// recorded split with no outcome, a stale outcome followed by a newer split,
/// a replan newer than its split, a replan older than its split, the empty
/// fold, and entries recorded under an unrelated branch — runs
/// `resumePlanFor`/`newestEntry` over them on
/// the real JIT with no agent and no git anywhere in the path, and asserts
/// what comes back against the decision table read directly out of
/// `Harness.hs`.
#[test]
fn dev_tree_resume_decisions_execute() {
    let json = execute_pure(
        "harness-dogfooding/dev-tree",
        outer_row_decls(),
        RESUME_DECISION_SOURCE,
        "__resumeDecisionReport",
    );
    let report = json
        .as_str()
        .expect("__resumeDecisionReport :: Text renders as a JSON string");
    let lines: Vec<&str> = report.lines().collect();
    let failures: Vec<&&str> = lines.iter().filter(|l| l.starts_with("FAIL")).collect();
    assert!(
        failures.is_empty(),
        "resume decision(s) diverged from the spec:\n{report}"
    );
    assert_eq!(
        lines.len(),
        18,
        "expected 18 resume-decision checks, got:\n{report}"
    );
}

/// The overspend regression the external review flagged
/// (`harness-dogfooding/dev-tree/Harness.hs:420-429`): `childAllowance`'s old
/// `max 1 ((parent.seedCycles - 2) \`div\` n)` minted a cycle from nothing
/// whenever the floor share rounded to zero. A non-leaf root with
/// `seedCycles = 2` and two leaf children passes `cycleRefusal` itself (its
/// own reservation is exactly met), but the OLD formula then handed each
/// child 1 cycle anyway — 2 (parent) + 1 + 1 = 4, double the parent's own
/// cap of 2.
///
/// The fix removes the upward clamp (`max 0` instead of `max 1`), so the same
/// scenario floors each child's share to 0; `requiredCycles` (1 for a leaf)
/// then exceeds that share, which is exactly the typed budget refusal
/// `cycleRefusal` already provides — no child spends anything. This pins
/// total spend at <= the parent's cap using ONLY `childAllowance` and
/// `requiredCycles` (both pure, no agent, no git), mirroring how
/// `cycleRefusal`/`allocateChildren` actually combine them in `decompose`.
const OVERSPEND_REGRESSION_SOURCE: &str = concat!(
    "{-# LANGUAGE NoImplicitPrelude, OverloadedStrings, DataKinds, TypeOperators, ",
    "FlexibleContexts, GADTs, ScopedTypeVariables, TypeApplications, LambdaCase, ",
    "RecordWildCards, OverloadedRecordDot, QuasiQuotes, DeriveGeneric, DeriveAnyClass #-}\n",
    "module OverspendProbe where\n",
    "import Tidepool.Prelude hiding (render)\n",
    "import Tidepool.Effects\n",
    "import Harness\n",
    "import HarnessTypes (OnFailure (..))\n",
    "import Tidepool.QQ (fmt)\n",
    "import qualified Data.Text as T\n",
    "\n",
    "leafPlan :: DevPlan\n",
    "leafPlan = DevPlan { nodeName = \"leaf\", nodeTask = \"t\", nodeChecks = [], nodeBoundary = [], nodeTolerated = [], nodeOnFailure = Retry, nodeSplit = Nothing, nodeScaffold = Nothing, childPlans = [], nodeCycles = Nothing }\n",
    "\n",
    "rootPlan :: DevPlan\n",
    "rootPlan = DevPlan { nodeName = \"root\", nodeTask = \"t\", nodeChecks = [], nodeBoundary = [], nodeTolerated = [], nodeOnFailure = Retry, nodeSplit = Nothing, nodeScaffold = Nothing, childPlans = [leafPlan, leafPlan], nodeCycles = Nothing }\n",
    "\n",
    "-- Never forced: 'childAllowance'/'requiredCycles' only read 'seedPlan' and\n",
    "-- 'seedCycles', so a live 'WorktreeHandle' is not needed for this pin.\n",
    "rootSeed :: NodeSeed\n",
    "rootSeed = NodeSeed { seedPlan = rootPlan, seedTree = undefined, seedDepth = 0, seedCycles = 2, seedAdopted = Nothing }\n",
    "\n",
    "nChildren :: Int\n",
    "nChildren = length (childPlans rootPlan)\n",
    "\n",
    "childShare :: Int\n",
    "childShare = childAllowance rootSeed nChildren\n",
    "\n",
    "-- A child only spends its share if it clears 'cycleRefusal' (the same test\n",
    "-- that function runs: seedCycles >= requiredCycles seedPlan). Below that\n",
    "-- threshold the child is refused and spends 0 — never the share itself.\n",
    "childSpend :: Int\n",
    "childSpend = if childShare >= requiredCycles leafPlan then childShare else 0\n",
    "\n",
    "totalSpend :: Int\n",
    "totalSpend = requiredCycles rootPlan + nChildren * childSpend\n",
    "\n",
    "__overspendReport :: Text\n",
    "__overspendReport =\n",
    "  T.intercalate \"\\n\"\n",
    "    [ [fmt|childShare={childShare}|]\n",
    "    , [fmt|totalSpend={totalSpend}|]\n",
    "    , [fmt|withinCap={totalSpend <= rootSeed.seedCycles}|]\n",
    "    ]\n",
);

/// Runs [`OVERSPEND_REGRESSION_SOURCE`] on the real JIT (pure, no agent, no
/// git) and asserts the exact numbers the review's scenario names: a floored
/// share of 0 per child, and total spend pinned at the parent's own
/// reservation (2) — never 4, which is what the pre-fix `max 1` formula
/// would have produced.
#[test]
fn dev_tree_child_allowance_never_overspends_parent_cap() {
    support::require_extract();
    let _cache_guard = support::isolate_cache();
    let cfg = EngineConfig::from_decls(
        outer_row_decls(),
        repo_root().join("haskell/lib"),
        Some(repo_root().join("harness-dogfooding/dev-tree")),
    )
    .expect("engine config for the dogfood row");
    let include: Vec<_> = cfg.include.iter().map(|p| p.as_path()).collect();
    let json =
        match compile_and_run_pure(OVERSPEND_REGRESSION_SOURCE, "__overspendReport", &include) {
            Ok(result) => result.to_json(),
            Err(e) => panic!("overspend regression probe did not run cleanly:\n{e}"),
        };
    let report = json
        .as_str()
        .expect("__overspendReport :: Text renders as a JSON string");
    assert!(report.contains("childShare=0"), "{report}");
    assert!(report.contains("totalSpend=2"), "{report}");
    assert!(report.contains("withinCap=True"), "{report}");
}

/// The journal vocabulary round-trip pin (dev-tree's own durable
/// `split`/`outcome`/`replan`/`rebase`/`escalation`/`micro-split`/
/// `micro-complete` schema, typed by
/// `DevTreeJournal`).
///
/// Every case below asserts TWO things about one `JournalEvent`: (1)
/// `payloadOf` builds the exact wire shape the pre-refactor code wrote by
/// hand at its `record` call site (a literal captured from that code, not
/// re-derived here), and (2) `decodeEvent` reconstructs the identical typed
/// value from that payload — so a journal the pre-refactor code wrote folds
/// identically under this reader. The escalation-defaulting and
/// unknown-kind cases pin the total-degrading contract `decodeEvent`'s
/// module doc promises, which nothing else here exercises.
const JOURNAL_ROUND_TRIP_SOURCE: &str = include_str!("probes/JournalRoundTripProbe.hs");

/// Runs [`JOURNAL_ROUND_TRIP_SOURCE`] on the real JIT (pure, no agent, no
/// git, no `Harness` effect at all — `recordEvent` itself is untested here
/// on purpose, since it is a one-line composition of `payloadOf`/`kindOf`
/// and "Tidepool.Journal"'s own `record`, already exercised end to end by
/// `selfharness_persistence`).
#[test]
fn dev_tree_journal_event_round_trips() {
    let json = execute_pure(
        "harness-dogfooding/dev-tree",
        outer_row_decls(),
        JOURNAL_ROUND_TRIP_SOURCE,
        "__journalRoundTripReport",
    );
    let report = json
        .as_str()
        .expect("__journalRoundTripReport :: Text renders as a JSON string");
    let lines: Vec<&str> = report.lines().collect();
    let failures: Vec<&&str> = lines.iter().filter(|l| l.starts_with("FAIL")).collect();
    assert!(
        failures.is_empty(),
        "journal event round-trip(s) diverged from the legacy wire:\n{report}"
    );
    assert_eq!(
        lines.len(),
        25,
        "expected 25 journal round-trip checks, got:\n{report}"
    );
}
